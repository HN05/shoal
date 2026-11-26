//! `shoal checkpoint` and `shoal restore --checkpoint`.
use crate::cli::ui::Acting;
use anyhow::{Context as _, Result, ensure};
use shoal_refs::BranchName;

use crate::client::recovery;
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    model::{Checkpoint, CheckpointCause, RestoredCheckpoint},
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn checkpoint(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    name: Option<String>,
) -> Result<i32> {
    let taking = taking();
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory(&taking));
    let workspace = workspace.await?;
    let taking = recovery::checkpoint(workspace.clone(), name).send(&ctx.paths);
    let checkpoint: Checkpoint = ctx.progress("Taking checkpoint", taking).await?;
    let files = if checkpoint.files_omitted {
        "; unsaved files include a repository or a changed submodule and were not kept"
    } else {
        ""
    };
    ctx.emit(
        &format!("Checkpoint {} saved{files}", checkpoint.number),
        &checkpoint,
    )?;
    Ok(0)
}

pub(super) async fn run(
    ctx: &Context,
    spaces: Vec<SpaceSelector>,
    checkpoint: Option<CheckpointNumber>,
) -> Result<i32> {
    ensure!(spaces.len() <= 1, "--checkpoint takes one workspace");
    let workspace = spaces.into_iter().next();
    let returning = returning();
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory(&returning));
    let workspace = workspace.await?;
    match checkpoint {
        Some(checkpoint) => restore(ctx, workspace, checkpoint).await,
        None => choose(ctx, workspace).await,
    }
}

/// Its name decides nothing of which workspaces it reaches.
pub(in crate::cli::commands) fn taking() -> Acting {
    Acting::request(|space| recovery::checkpoint(space, None))
}

/// The checkpoint, which may be picked next, decides nothing of which
/// workspaces it reaches.
pub(in crate::cli::commands) fn returning() -> Acting {
    Acting::request(|space| recovery::restore_checkpoint(space, CheckpointNumber::new(1)))
}

async fn choose(ctx: &Context, workspace: SpaceSelector) -> Result<i32> {
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
async fn list(ctx: &Context, workspace: &SpaceSelector) -> Result<Option<Vec<Checkpoint>>> {
    recovery::checkpoints(&ctx.paths, workspace).await
}

async fn restore(
    ctx: &Context,
    workspace: SpaceSelector,
    checkpoint: CheckpointNumber,
) -> Result<i32> {
    let restored: RestoredCheckpoint = ctx
        .progress(
            "Restoring checkpoint",
            recovery::restore_checkpoint(workspace, checkpoint).send(&ctx.paths),
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
        CheckpointCause::Refresh => "before refresh",
    }
}

pub(super) fn branch_label(branch: Option<&BranchName>) -> &str {
    branch.map_or("detached HEAD", BranchName::as_str)
}
