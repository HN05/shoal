//! The stopped work and last sessions a restore of agents acts on.
use anyhow::{Result, ensure};

use super::targets::Selected;
use crate::{
    cli::context::Context,
    execution::recovery::Saved,
    model::{QueueEntry, Space},
};
use shoal_protocol::space_names::SpaceSelector;

/// The stopped work and last sessions of the `selected` spaces, refusing
/// one with nothing to resume, or of every space with any.
pub(in crate::cli::commands) async fn stopped(
    ctx: &Context,
    selected: &Selected,
) -> Result<Vec<(Space, Saved)>> {
    let Some(selectors) = &selected.spaces else {
        return stopped_work(ctx, false).await;
    };
    let mut stopped = Vec::new();
    for selector in selectors {
        let space = super::present_space(ctx, selector).await?;
        let saved = saved_work(ctx, &space, false).await?;
        ensure!(
            !saved.is_empty(),
            "{} has no stopped agent or commands to resume",
            space.name
        );
        stopped.push((space, saved));
    }
    Ok(stopped)
}

/// The stopped work and last sessions of the `selected` spaces that a
/// discard takes, or of every space with any, refusing a space with neither
/// those nor one of the failed `launches`.
pub(in crate::cli::commands) async fn discardable(
    ctx: &Context,
    selected: &Selected,
    launches: &[QueueEntry],
) -> Result<Vec<(Space, Saved)>> {
    let Some(selectors) = &selected.spaces else {
        return stopped_work(ctx, true).await;
    };
    let mut stopped = Vec::new();
    for selector in selectors {
        let space = super::present_space(ctx, selector).await?;
        let saved = saved_work(ctx, &space, true).await?;
        let launched = launches
            .iter()
            .any(|entry| entry.space() == Some(&space.id));
        ensure!(
            !saved.is_empty() || launched,
            "{} has no stopped agent, commands or failed launch to discard",
            space.name
        );
        if !saved.is_empty() {
            stopped.push((space, saved));
        }
    }
    Ok(stopped)
}

/// The spaces this process reaches with stopped work or last sessions, as
/// [`saved_work`] keeps them for `discard`. A done assignment needs no
/// agent, so its space is included only to `discard`.
async fn stopped_work(ctx: &Context, discard: bool) -> Result<Vec<(Space, Saved)>> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let mut stopped = Vec::new();
    for space in crate::scoping::Scoping::inherited(&ctx.paths.state).reached_below(spaces) {
        if space.done() && !discard {
            continue;
        }
        let saved = saved_work(ctx, &space, discard).await?;
        if !saved.is_empty() {
            stopped.push((space, saved));
        }
    }
    Ok(stopped)
}

/// The stopped work and last sessions of `space`: with the agents waiting
/// in their wrappers, which a restore starts there, except to `discard`,
/// which leaves them to their wrappers.
async fn saved_work(ctx: &Context, space: &Space, discard: bool) -> Result<Saved> {
    let mut saved = crate::client::saved::read(&ctx.paths, SpaceSelector::id(&space.id)).await?;
    if discard {
        saved.retain_unattended();
    }
    Ok(saved)
}
