//! A space whose agent's launch failed to start restores by queuing that
//! launch again exactly as it was: as the queue admits it, at once for
//! `--now`, or in this terminal for `--focus`.
use anyhow::Result;

use super::permission_change;
use crate::cli::commands::restore::targets::Selected;
use crate::client::output::batch::Failure;
use crate::{
    cli::{commands::queued_agents, context::Context, ui},
    config::permissions::PermissionChange,
    model::QueueEntry,
    state::QueueState,
};
use shoal_protocol::protocol::RetryStart;

/// Move the selected spaces with a failed launch from `selected`'s spaces
/// to its failed starts; with every space selected, take every failed
/// launch when `every`, otherwise none.
pub(in crate::cli::commands) async fn take(
    ctx: &Context,
    selected: &mut Selected,
    every: bool,
) -> Result<()> {
    let Some(selectors) = selected.spaces.take() else {
        if every {
            let entries = queued_agents::entries(ctx).await?.into_iter();
            let failed = entries.filter(|entry| entry.state == QueueState::Failed);
            selected.failed_starts = failed.collect();
        }
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
    let start = match now {
        true => RetryStart::Now,
        false => RetryStart::Queued,
    };
    crate::client::queue::retry(&ctx.paths, entry.id.clone(), start).await
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
    let retried =
        (crate::client::queue::retry(&ctx.paths, failed.id.clone(), RetryStart::Here)).await?;
    let _held = queued_agents::Hold::start(&ctx.paths, &retried.id).await;
    let entry = crate::client::queue::start_here(&ctx.paths, &retried.id).await?;
    queued_agents::run_here(ctx, &entry.id).await
}
