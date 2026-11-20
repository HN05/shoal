//! The stopped work and last sessions a restore of agents acts on.
use anyhow::{Result, ensure};

use crate::cli::commands::restore::targets::Selected;
use crate::{cli::context::Context, execution::recovery::Saved, model::Space};
use shoal_protocol::space_names::SpaceSelector;

/// The stopped work and last sessions of the `selected` spaces, or of every
/// space with any, as [`saved_work`] keeps them for `discard`.
pub(super) async fn stopped(
    ctx: &Context,
    selected: &Selected,
    discard: bool,
) -> Result<Vec<(Space, Saved)>> {
    match &selected.spaces {
        None => stopped_work(ctx, discard).await,
        Some(spaces) => stopped_in(ctx, spaces, discard).await,
    }
}

/// The stopped work and last sessions of the spaces `selectors` name, as
/// [`saved_work`] keeps them for `discard`, refusing one with nothing to
/// resume.
async fn stopped_in(
    ctx: &Context,
    selectors: &[SpaceSelector],
    discard: bool,
) -> Result<Vec<(Space, Saved)>> {
    let mut stopped = Vec::new();
    for selector in selectors {
        let space = super::present_space(ctx, selector).await?;
        let saved = saved_work(ctx, &space, discard).await?;
        ensure!(
            !saved.is_empty(),
            "{} has no stopped agent or commands to {}",
            space.name,
            if discard { "discard" } else { "restore" }
        );
        stopped.push((space, saved));
    }
    Ok(stopped)
}

/// The spaces this process reaches with stopped work or last sessions, as
/// [`saved_work`] keeps them for `discard`. A done assignment needs no
/// agent, so its space is included only to `discard`.
async fn stopped_work(ctx: &Context, discard: bool) -> Result<Vec<(Space, Saved)>> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let mut stopped = Vec::new();
    for space in crate::cli::scoping::Scoping::inherited(&ctx.paths.state).reached_below(spaces) {
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
