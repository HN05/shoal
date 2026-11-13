//! The stopped work and last sessions a restore of agents acts on.
use anyhow::{Result, ensure};

use super::super::targets::Selected;
use crate::client::space_names::SpaceSelector;
use crate::{cli::context::Context, execution::recovery::Saved, model::PresentSpace};

/// The stopped work and last sessions of the `selected` spaces, or of every
/// space with any, as [`saved_work`] keeps them for `forget`.
pub(super) async fn stopped(
    ctx: &Context,
    selected: &Selected,
    forget: bool,
) -> Result<Vec<(PresentSpace, Saved)>> {
    match &selected.spaces {
        None => stopped_work(ctx, forget).await,
        Some(spaces) => stopped_in(ctx, spaces, forget).await,
    }
}

/// The stopped work and last sessions of the spaces `selectors` name, as
/// [`saved_work`] keeps them for `forget`, refusing one with nothing to
/// resume.
async fn stopped_in(
    ctx: &Context,
    selectors: &[SpaceSelector],
    forget: bool,
) -> Result<Vec<(PresentSpace, Saved)>> {
    let mut stopped = Vec::new();
    for selector in selectors {
        let space = super::present_space(ctx, selector).await?;
        let saved = saved_work(ctx, &space, forget).await?;
        ensure!(
            !saved.is_empty(),
            "{} has no stopped agents or commands to restore",
            space.name()
        );
        stopped.push((space, saved));
    }
    Ok(stopped)
}

/// The spaces this process reaches with stopped work or last sessions, as
/// [`saved_work`] keeps them for `forget`. A done assignment needs no
/// agent, so its space is included only to `forget`.
async fn stopped_work(ctx: &Context, forget: bool) -> Result<Vec<(PresentSpace, Saved)>> {
    let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
    let mut stopped = Vec::new();
    for space in crate::cli::scoping::Scoping::inherited(&ctx.paths.state).reached_below(spaces) {
        if space.done() && !forget {
            continue;
        }
        let saved = saved_work(ctx, &space, forget).await?;
        if !saved.is_empty() {
            stopped.push((space, saved));
        }
    }
    Ok(stopped)
}

/// The stopped work and last sessions of `space`: with the agents waiting
/// in their wrappers, which a restore starts there, except to `forget`,
/// which leaves them to their wrappers.
async fn saved_work(ctx: &Context, space: &PresentSpace, forget: bool) -> Result<Saved> {
    let mut saved = crate::client::saved::read(&ctx.paths, SpaceSelector::id(space.id())).await?;
    if forget {
        saved.retain_unattended();
    }
    Ok(saved)
}
