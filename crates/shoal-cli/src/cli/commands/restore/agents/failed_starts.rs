//! A space whose agent's launch failed to start restores by queuing that
//! launch again exactly as it was: as the queue admits it, or at once for
//! `--now`.
use anyhow::Result;

use super::super::targets::Selected;
use crate::client::output::batch::Failure;
use crate::{
    cli::{commands::queued_agents, context::Context, ui},
    model::QueueEntry,
    state::QueueState,
};

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
    let retried = crate::client::queue::retry(&ctx.paths, entry.id.clone()).await?;
    match now && retried.state == QueueState::Queued {
        true => crate::client::queue::start(&ctx.paths, retried.id.to_string()).await,
        false => Ok(retried),
    }
}
