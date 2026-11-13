//! Checkpoints of workspaces: taking one, and returning a workspace to one.
use anyhow::{Context as _, Result};
use shoal_refs::BranchName;

use crate::client::recovery;
use crate::client::space_names::SpaceSelector;
use crate::model::CheckpointNumber;
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    model::{Checkpoint, CheckpointCause, RestoredCheckpoint},
};

/// `shoal checkpoint`: keep the workspace's current state.
pub(in crate::cli::commands) async fn checkpoint(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    name: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let checkpoint: Checkpoint = ctx
        .progress(
            "Taking checkpoint",
            recovery::checkpoint(&ctx.paths, workspace.clone(), name),
        )
        .await?;
    let files = if checkpoint.files_omitted {
        "; unsaved files exceed the archive limit and were not kept"
    } else {
        ""
    };
    ctx.emit(
        &format!("Checkpoint {} saved{files}", checkpoint.number),
        &checkpoint,
    )?;
    Ok(0)
}

/// Return the workspace to the checkpoint the user picks, or list its
/// checkpoints when there is no terminal or none to pick.
pub(super) async fn choose(ctx: &Context, workspace: SpaceSelector) -> Result<i32> {
    let checkpoints =
        (list(ctx, &workspace).await?).with_context(|| format!("no workspace {workspace}"))?;
    if checkpoints.is_empty() || !ctx.interactive() {
        ctx.show(&checkpoints, |checkpoints| print_checkpoints(checkpoints))?;
        return Ok(0);
    }
    let entries = checkpoints
        .iter()
        .map(|checkpoint| checkpoint.number.to_string())
        .zip(checkpoint_lines(&checkpoints))
        .collect();
    let checkpoint = ui::pick(ctx, "Checkpoint> ", entries)?
        .parse()
        .context("picker returned an invalid checkpoint")?;
    restore(ctx, workspace, checkpoint).await
}

/// Every checkpoint of an existing workspace, requested page by page;
/// `None` when no workspace matches.
pub(super) async fn list(
    ctx: &Context,
    workspace: &SpaceSelector,
) -> Result<Option<Vec<Checkpoint>>> {
    recovery::checkpoints(&ctx.paths, workspace).await
}

pub(super) async fn restore(
    ctx: &Context,
    workspace: SpaceSelector,
    checkpoint: CheckpointNumber,
) -> Result<i32> {
    let restored: RestoredCheckpoint = ctx
        .progress(
            "Restoring checkpoint",
            recovery::restore_checkpoint(&ctx.paths, workspace, checkpoint),
        )
        .await?;
    let files = if restored.checkpoint.files_omitted {
        "; its unsaved files were not kept"
    } else {
        ""
    };
    ctx.emit(
        &format!(
            "Restored {} to checkpoint {}{files}; checkpoint {} keeps the state before it",
            Palette::stdout(ctx.json).paint(Style::Heading, &restored.workspace.name),
            restored.checkpoint.number,
            restored.replaced.number
        ),
        &restored,
    )?;
    Ok(0)
}

fn print_checkpoints(checkpoints: &[Checkpoint]) {
    if checkpoints.is_empty() {
        outln!("No checkpoints");
    }
    for line in checkpoint_lines(checkpoints) {
        outln!("{line}");
    }
}

/// One aligned line per checkpoint: its number, time, cause, branch and name.
fn checkpoint_lines(checkpoints: &[Checkpoint]) -> Vec<String> {
    let width = checkpoints
        .last()
        .map_or(1, |checkpoint| checkpoint.number.to_string().len());
    checkpoints
        .iter()
        .map(|checkpoint| {
            let name = checkpoint
                .name
                .as_deref()
                .map(|name| format!("  {name}"))
                .unwrap_or_default();
            let files = if checkpoint.files_omitted {
                "  (unsaved files not kept)"
            } else {
                ""
            };
            format!(
                "{:>width$}  {}  {}  {}{name}{files}",
                checkpoint.number,
                checkpoint.created_at.local_seconds(),
                cause_label(checkpoint.cause),
                branch_label(checkpoint.branch.as_ref()),
            )
        })
        .collect()
}

fn cause_label(cause: CheckpointCause) -> &'static str {
    match cause {
        CheckpointCause::Manual => "manual",
        CheckpointCause::AgentStopped => "agent stopped",
        CheckpointCause::Restore => "before restore",
        CheckpointCause::Removal => "removal",
        CheckpointCause::Start => "start of copy",
    }
}

pub(super) fn branch_label(branch: Option<&BranchName>) -> &str {
    branch.map_or("detached HEAD", BranchName::as_str)
}
