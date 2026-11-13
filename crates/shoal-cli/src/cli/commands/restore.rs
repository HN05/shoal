//! `shoal restore`: opening removed workspaces again, and returning
//! workspaces to their checkpoints.
use anyhow::{Result, ensure};

use crate::cli::{
    context::Context,
    ui::{self, Fallback},
};
use crate::client::space_names::SpaceSelector;
use crate::model::CheckpointNumber;

/// `shoal restore`: open a removed workspace again, or choose a removed
/// one, or with `checkpoint` return a workspace to one of its checkpoints,
/// or choose one when it names none. Without a terminal, the choices are
/// listed instead.
pub(super) async fn run(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    checkpoint: Option<Option<CheckpointNumber>>,
) -> Result<i32> {
    if let Some(checkpoint) = checkpoint {
        let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
        return match checkpoint {
            Some(checkpoint) => checkpoints::restore(ctx, workspace, checkpoint).await,
            None => checkpoints::choose(ctx, workspace).await,
        };
    }
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot reopen removed workspaces; shoal restore --checkpoint lists your workspace's checkpoints"
    );
    let Some(workspace) = workspace else {
        return reopen::choose_removed(ctx).await;
    };
    ensure!(
        checkpoints::list(ctx, &workspace).await?.is_none(),
        "{workspace} is not removed; shoal restore {workspace} --checkpoint lists its checkpoints"
    );
    reopen::reopen(ctx, workspace).await
}

pub(super) mod agents;
mod checkpoints;
pub(super) mod reopen;

pub(super) use checkpoints::checkpoint;
