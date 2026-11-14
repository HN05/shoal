//! `shoal restore`: restoring what of a space is stopped, interrupted or
//! removed, and returning workspaces to their checkpoints.
use anyhow::Result;

use crate::cli::{args::SpaceTargets, context::Context};
use crate::model::CheckpointNumber;

/// `shoal restore`: return the workspace `targets` names to `checkpoint`,
/// or choose one when it names none; otherwise restore the spaces `targets`
/// selects, once, each as its kind restores: a removed workspace opens
/// again, and the stopped agents of the others restore as `options` says.
/// Without a terminal, the choices are listed instead.
pub(super) async fn run(
    ctx: &Context,
    targets: SpaceTargets,
    checkpoint: Option<Option<CheckpointNumber>>,
    options: agents::Options,
) -> Result<i32> {
    if let Some(checkpoint) = checkpoint {
        return checkpoints::run(ctx, targets.space, checkpoint).await;
    }
    if crate::env::inherits_scope(&ctx.paths.state) {
        agents::ensure_queued()?;
    }
    let named = !targets.space.is_empty();
    match targets::Selected::load(ctx, targets).await? {
        Some(selected) => agents::restore(ctx, selected, named, options).await,
        None => Ok(0),
    }
}

pub(super) mod agents;
mod checkpoints;
pub(super) mod reopen;
mod targets;

pub(super) use checkpoints::checkpoint;
