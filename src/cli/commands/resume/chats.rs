//! `shoal resume` for the stopped chats: each comes back under its own
//! name, continuing its session, in a background place where a restored
//! agent opens, or in this terminal with `--focus`, with the permissions it
//! was given and what `--permission` changes in them; `--discard` removes
//! it instead.
use crate::client::space_names::SpaceSelector;
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        agents::ChatRepository,
        commands::chat::{self, ChatPlace, Started},
        context::Context,
        ui,
    },
    client::output::{batch::Failure, resume::RestoredChat},
    config::permissions::PermissionChange,
    model::{Chat, Spaces},
};

/// Restore the stopped `chat` in this terminal, making `change` when given
/// to the permissions it was given.
pub(super) async fn restore_here(
    ctx: &Context,
    chat: &Chat,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    match restore(ctx, chat, ChatPlace::Here, change).await? {
        Started::Exited(code) => Ok(code),
        Started::Background(_) => Ok(0),
    }
}

/// Remove each of the stopped `chats` without restoring it, returning
/// those it removed and those it failed for. One with spaces below it,
/// which its removal would take, is left to `shoal rm`, which shows them.
pub(super) async fn discard_each(ctx: &Context, chats: &[Chat]) -> (Vec<Chat>, Vec<Failure>) {
    let mut report = ui::Report::new("discard");
    let spaces = match crate::client::workspaces::spaces(&ctx.paths).await {
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
        .map(|space| space.name.into())
        .collect();
    ensure!(
        below.is_empty(),
        "removing it also removes the spaces below it ({}); remove it with shoal rm {}",
        below.join(", "),
        chat.name
    );
    Ok(
        crate::client::chats::remove(&ctx.paths, &chat.id, Some(Vec::new()))
            .await?
            .chat,
    )
}

/// Restore each of the stopped `chats` in the background, making `change`
/// when given, returning those it restored and those it failed for.
pub(super) async fn restore_each(
    ctx: &Context,
    chats: &[Chat],
    change: Option<&PermissionChange>,
) -> (Vec<RestoredChat>, Vec<Failure>) {
    let mut report = ui::Report::new("restore");
    for chat in chats {
        match restore(ctx, chat, ChatPlace::Background, change).await {
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
/// continuing the session it reported, with the permissions it was given
/// and `change` made to them when given. A failure here, such as a session this
/// process starts for an app, reaches the user through the daemon, as one in
/// the place it opens does.
async fn restore(
    ctx: &Context,
    chat: &Chat,
    place: ChatPlace,
    change: Option<&PermissionChange>,
) -> Result<Started> {
    let started = start_saved(ctx, chat, place, change).await;
    crate::client::chats::keep_failed_restore(&ctx.paths, Some(&chat.id), started).await
}

/// [`restore`] without the report of its failure.
async fn start_saved(
    ctx: &Context,
    chat: &Chat,
    place: ChatPlace,
    change: Option<&PermissionChange>,
) -> Result<Started> {
    let repo = saved_repository(ctx, chat).await?;
    let settings = crate::client::workspaces::settings(&ctx.paths, repo.config()).await?;
    // The restored agent keeps its mode's rules; its subject was sent once.
    let topic = match (&chat.workspace, &repo) {
        (Some(workspace), _) => chat::Topic::workspace(
            chat::hosting_workspace(ctx, &SpaceSelector::id(&workspace.id)).await?,
            &chat.mode,
        ),
        (None, ChatRepository::Registered(repo)) => chat::Topic::repository(repo, &chat.mode),
        (None, ChatRepository::Outside(_))
            if chat.mode == crate::config::modes::ModeName::SETUP =>
        {
            super::super::config_setup::topic(&crate::config::Config::path(&ctx.paths))
        }
        (None, ChatRepository::Outside(_)) => chat::Topic::outside_repositories(&chat.mode),
    };
    // A converted chat starts from a fork of its session in the checkout,
    // where the session never ran.
    let topic = match (chat.converted, &repo) {
        (true, ChatRepository::Registered(registered)) => topic.converted(&registered.path),
        _ => topic,
    };
    let options = chat::Options {
        narrowing: chat::Narrowing::Recorded(match change {
            Some(change) => change.applied_to(chat.narrowing),
            None => chat.narrowing,
        }),
        agent: Some(chat.agent.name().parse()?),
        place,
        name: Some(chat.name.clone()),
        model: Default::default(),
        args: Vec::new(),
        session: chat.session.clone(),
        fork: chat.converted,
        restore: Some(chat.id.clone()),
        handoff: None,
        parent: None,
    };
    chat::launch(ctx, repo, settings, topic, options).await
}

/// Where the `chat` starts again: its registered repository, or the
/// `root_dir` a chat without one started in.
pub(super) async fn saved_repository(ctx: &Context, chat: &Chat) -> Result<ChatRepository> {
    let Some(id) = &chat.repository_id else {
        let root_dir = match &chat.root_dir {
            Some(root_dir) => root_dir.clone(),
            None => chat::root_dir(ctx).await?,
        };
        return Ok(ChatRepository::Outside(root_dir));
    };
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let repo = (repositories.into_iter())
        .find(|repo| repo.id == *id)
        .with_context(|| format!("the repository of chat {} is missing", chat.name))?;
    Ok(ChatRepository::Registered(repo))
}
