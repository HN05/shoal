//! A space whose agent's launch failed to start restores by queuing that
//! launch again exactly as it was: as the queue admits it, at once for
//! `--now`, or in this terminal for `--focus`.
use anyhow::{Context as _, Result};

use super::super::targets::Selected;
use super::permission_change;
use crate::client::output::batch::Failure;
use crate::{
    cli::{commands::queued_agents, context::Context, ui},
    config::permissions::PermissionChange,
    model::{QueueEntry, Space},
    state::QueueState,
};
use shoal_protocol::space_names::SpaceSelector;

/// Move the selected spaces with a failed launch from `selected`'s spaces
/// to its failed starts.
pub(in crate::cli::commands::restore) async fn take(
    ctx: &Context,
    selected: &mut Selected,
) -> Result<()> {
    let Some(selectors) = selected.spaces.take() else {
        return Ok(());
    };
    let entries = queued_agents::entries(ctx).await?;
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let mut rest = Vec::new();
    for selector in selectors {
        let space = spaces.find(&selector);
        let failed = (entries.iter()).find(|entry| {
            entry.state == QueueState::Failed
                && space
                    .as_ref()
                    .is_some_and(|space| entry.space() == Some(&space.id))
        });
        match failed {
            Some(failed) => selected.failed_starts.push(failed.clone()),
            None => rest.push(selector),
        }
    }
    selected.spaces = Some(rest);
    Ok(())
}

/// Queue each of the failed launches `failed` again, starting it at once
/// when `now`, going on after one fails, which it reports.
pub(super) async fn retry_each(
    ctx: &Context,
    failed: &[QueueEntry],
    now: bool,
) -> (Vec<QueueEntry>, Vec<Failure>) {
    let (mut retried, mut failures) = (Vec::new(), Vec::new());
    for entry in failed {
        match retry(ctx, entry, now).await {
            Ok(entry) => retried.push(entry),
            Err(error) => failures.push(ui::failure(ctx, "restore", &entry.name, &error)),
        }
    }
    (retried, failures)
}

/// Queue the failed launch `entry` again, starting it at once when `now`.
async fn retry(ctx: &Context, entry: &QueueEntry, now: bool) -> Result<QueueEntry> {
    let retried = crate::client::queue::retry(&ctx.paths, entry.id.clone(), false).await?;
    match now && retried.state == QueueState::Queued {
        true => crate::client::queue::start(&ctx.paths, retried.id.to_string()).await,
        false => Ok(retried),
    }
}

/// Queue the failed launch `failed` again and start its agent in this
/// terminal, which the queue leaves to this process meanwhile, making
/// `change` to the permissions it starts with.
pub(super) async fn retry_here(
    ctx: &Context,
    failed: &QueueEntry,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let launches = (&[][..], std::slice::from_ref(failed));
    permission_change::apply(ctx, &[], launches, change).await?;
    let retried = crate::client::queue::retry(&ctx.paths, failed.id.clone(), true).await?;
    let _held = queued_agents::Hold::start(&ctx.paths, &retried.id).await;
    let entry = crate::client::queue::start_here(&ctx.paths, &retried.id).await?;
    match &entry.opens {
        Some(_) => queued_agents::run_here(ctx, &entry.id).await,
        None => run_chat_here(ctx, &entry).await,
    }
}

/// Run here the agent of the chat the starting launch `entry`, which this
/// process holds, starts in, as the queue's start would in a place.
async fn run_chat_here(ctx: &Context, entry: &QueueEntry) -> Result<i32> {
    let chat = (entry.space().cloned()).context("the restored launch names no chat")?;
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let found = spaces.find(&SpaceSelector::id(&chat)).map(Space::into_chat);
    let chat = found
        .and_then(Result::ok)
        .context("the restored launch's chat is gone")?;
    let queued = crate::client::queue::plan(&ctx.paths, entry.id.clone()).await?;
    let launch = crate::start_plan::QueuedStart::of(&queued.plan)?.launch;
    crate::env::launch_queue_entry(&entry.id);
    let start = crate::queued_starts::chat_start(&chat, launch);
    let handed = crate::chat_handoff::HandedStart {
        start,
        chat: chat.id.clone(),
    };
    crate::cli::commands::start::start_resolved(ctx, handed).await
}
