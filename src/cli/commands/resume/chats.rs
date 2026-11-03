//! `shoal resume` for the stopped chats: each comes back under its own
//! name, continuing its session, in a background place where a restored
//! agent opens, or in this terminal with `--focus`; `--discard` removes it
//! instead.
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        client,
        commands::chat::{self, ChatPlace, Started},
        context::Context,
        ui,
    },
    client::output::{batch::Failure, resume::RestoredChat},
    model::{Chat, Spaces},
};

/// Restore the stopped `chat` in this terminal.
pub(super) async fn restore_here(ctx: &Context, chat: &Chat) -> Result<i32> {
    match restore(ctx, chat, ChatPlace::Here).await? {
        Started::Exited(code) => Ok(code),
        Started::Background(_) => Ok(0),
    }
}

/// Remove each of the stopped `chats` without restoring it, returning
/// those it removed and those it failed for. One with spaces below it,
/// which its removal would take, is left to `shoal rm`, which shows them.
pub(super) async fn discard_each(ctx: &Context, chats: &[Chat]) -> (Vec<Chat>, Vec<Failure>) {
    let mut report = ui::Report::new("discard");
    let spaces = match client::spaces(&ctx.paths).await {
        Ok(spaces) => spaces,
        Err(error) => {
            for chat in chats {
                report.failed(ctx, &chat.name, &error);
            }
            return report.into_parts();
        }
    };
    for chat in chats {
        match discard(ctx, chat, &spaces).await {
            Ok(removed) => report.done(ctx, &format!("Removed chat {}", removed.name), removed),
            Err(error) => report.failed(ctx, &chat.name, &error),
        }
    }
    report.into_parts()
}

/// Remove the stopped `chat` unless spaces are below it.
async fn discard(ctx: &Context, chat: &Chat, spaces: &Spaces) -> Result<Chat> {
    let below: Vec<String> = (spaces.below(&chat.id).into_iter())
        .map(|space| space.name)
        .collect();
    ensure!(
        below.is_empty(),
        "removing it also removes the spaces below it ({}); remove it with shoal rm {}",
        below.join(", "),
        chat.name
    );
    Ok(client::remove_chat(&ctx.paths, &chat.id, Some(Vec::new()))
        .await?
        .chat)
}

/// Restore each of the stopped `chats` in the background, returning those it
/// restored and those it failed for.
pub(super) async fn restore_each(
    ctx: &Context,
    chats: &[Chat],
) -> (Vec<RestoredChat>, Vec<Failure>) {
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
    report.into_parts()
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

/// Start the stopped `chat` again at `place`, in its repository's checkout
/// or the worktree of its workspace, under its own name and agent,
/// continuing the session it reported. A
/// failure here, such as a session this process starts for an app, reaches
/// the user through the daemon, as one in the place it opens does.
async fn restore(ctx: &Context, chat: &Chat, place: ChatPlace) -> Result<Started> {
    let started = start_saved(ctx, chat, place).await;
    client::keep_failed_restore(&ctx.paths, Some(&chat.id), started).await
}

/// [`restore`] without the report of its failure.
async fn start_saved(ctx: &Context, chat: &Chat, place: ChatPlace) -> Result<Started> {
    let repo = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == chat.repository_id)
        .with_context(|| format!("the repository of chat {} is missing", chat.name))?;
    let settings = chat::settings(ctx, &repo).await?;
    let topic = match &chat.workspace {
        Some(workspace) => chat::Topic::workspace(chat::find_workspace(ctx, &workspace.id).await?),
        None => chat::Topic::repository(&repo),
    };
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
