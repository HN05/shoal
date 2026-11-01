//! `shoal resume` for the chats Shoal saved: each comes back under its own
//! name, continuing its session, in a background place where a restored
//! agent opens, or in this terminal with `--focus`; `--discard` ends it
//! instead.
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        args::{SpaceTargets, WorkspaceTargets},
        client,
        commands::chat::{self, ChatPlace, Started},
        context::Context,
        ui,
    },
    client::output::resume::{DiscardedChats, RestoredChat, RestoredChats},
    model::Chat,
};

/// The saved chats `targets` name, by ID or name, and the targets left,
/// which name workspaces. Workspace processes see no chats.
pub async fn split(ctx: &Context, targets: SpaceTargets) -> Result<(Vec<Chat>, WorkspaceTargets)> {
    let SpaceTargets { space, all } = targets;
    if space.is_empty() {
        return Ok((
            Vec::new(),
            WorkspaceTargets {
                workspace: space,
                all,
            },
        ));
    }
    let saved: Vec<Chat> = client::recorded_chats(&ctx.paths)
        .await?
        .into_iter()
        .filter(|chat| chat.restore.is_some())
        .collect();
    let mut chats = Vec::new();
    let mut workspace = Vec::new();
    for selector in space {
        match saved
            .iter()
            .find(|chat| chat.id == selector || chat.name == selector)
        {
            Some(chat) => chats.push(chat.clone()),
            None => workspace.push(selector),
        }
    }
    Ok((chats, WorkspaceTargets { workspace, all }))
}

/// Restore the saved `chats`, in this terminal for `focus`, or end them
/// for `discard`. `others` are the other targets, which must name none.
pub async fn resume(
    ctx: &Context,
    chats: Vec<Chat>,
    others: &WorkspaceTargets,
    execution: Option<&str>,
    discard: bool,
    focus: bool,
) -> Result<i32> {
    ensure!(
        others.workspace.is_empty(),
        "resume chats and workspaces separately"
    );
    ensure!(
        execution.is_none(),
        "--execution selects a stopped execution of a workspace"
    );
    if focus {
        let [chat] = chats.as_slice() else {
            anyhow::bail!("--focus restores one chat");
        };
        return match restore(ctx, chat, ChatPlace::Here).await? {
            Started::Exited(code) => Ok(code),
            Started::Background(_) => Ok(0),
        };
    }
    match discard {
        true => discard_each(ctx, &chats).await,
        false => restore_each(ctx, &chats).await,
    }
}

/// End each of the saved `chats` without restoring it.
async fn discard_each(ctx: &Context, chats: &[Chat]) -> Result<i32> {
    let mut report = ui::Report::new("discard");
    for chat in chats {
        match client::discard_chat(&ctx.paths, &chat.id).await {
            Ok(discarded) => report.done(
                ctx,
                &format!("Discarded chat {}", discarded.name),
                discarded,
            ),
            Err(error) => report.failed(ctx, &chat.name, &error),
        }
    }
    report.finish(ctx, "", |discarded, failed| DiscardedChats {
        discarded,
        failed,
    })
}

/// Restore each of the saved `chats` in the background.
async fn restore_each(ctx: &Context, chats: &[Chat]) -> Result<i32> {
    let mut report = ui::Report::new("restore");
    for chat in chats {
        match restore(ctx, chat, ChatPlace::Background).await {
            Ok(started) => {
                let restored = restored(chat, started);
                let message = match &restored.place {
                    Some(place) => format!("Restored chat {}. {place}", restored.chat),
                    None => format!("Restored chat {}", restored.chat),
                };
                report.done(ctx, &message, restored);
            }
            Err(error) => report.failed(ctx, &chat.name, &error),
        }
    }
    report.finish(ctx, "", |restored, failed| RestoredChats {
        restored,
        failed,
    })
}

/// What a resume reports of `chat`, which started as `started`.
fn restored(chat: &Chat, started: Started) -> RestoredChat {
    match started {
        Started::Background(started) => RestoredChat {
            chat: started.chat,
            agent: started.agent,
            repository: started.repository,
            place: Some(started.place),
        },
        Started::Exited(_) => RestoredChat {
            chat: chat.name.clone(),
            agent: chat.agent.clone(),
            repository: chat.repository_id.clone(),
            place: None,
        },
    }
}

/// Start the saved `chat` again at `place`, in its repository's checkout,
/// under its own name and agent, continuing the session it reported.
async fn restore(ctx: &Context, chat: &Chat, place: ChatPlace) -> Result<Started> {
    let repo = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == chat.repository_id)
        .with_context(|| format!("the repository of chat {} is missing", chat.name))?;
    let settings = chat::settings(ctx, &repo).await?;
    let topic = chat::Topic::repository(&repo);
    let options = chat::Options {
        agent: Some(chat.agent.parse()?),
        place,
        name: Some(chat.name.clone()),
        model: Default::default(),
        args: Vec::new(),
        session: chat.session.clone(),
        restore: Some(chat.id.clone()),
    };
    chat::launch(ctx, repo, settings, topic, options).await
}
