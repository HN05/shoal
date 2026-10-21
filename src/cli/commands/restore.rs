//! Checkpoints of workspaces, and opening removed workspaces again from their
//! removal records.
use anyhow::{Result, ensure};
use serde_json::json;

use crate::{
    cli::{
        client::request,
        context::Context,
        output::{Palette, Style},
        terminal,
        ui::{self, Fallback},
    },
    model::{Checkpoint, CheckpointCause, RemovedWorkspace, RestoredCheckpoint, RestoredWorkspace},
    protocol::Method,
    shell,
    state::WorkspaceState,
    time::local_seconds,
};

/// `shoal checkpoint`: keep the workspace's current state.
pub(super) async fn checkpoint(
    ctx: &Context,
    workspace: Option<String>,
    name: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let checkpoint: Checkpoint = ctx
        .progress(
            "Taking checkpoint",
            request(&ctx.paths, Method::Checkpoint { workspace, name }),
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

/// `shoal restore`: return an existing workspace to a checkpoint or list its
/// checkpoints, open a removed workspace again, or list the removed ones.
pub(super) async fn run(
    ctx: &Context,
    workspace: Option<String>,
    checkpoint: Option<u64>,
) -> Result<i32> {
    let scoped = crate::env::inherits_scope(&ctx.paths.state);
    let Some(workspace) = workspace else {
        ensure!(
            !scoped,
            "workspace processes cannot reopen removed workspaces; shoal restore <workspace> lists your workspace's checkpoints"
        );
        let removed =
            request::<Vec<RemovedWorkspace>>(&ctx.paths, Method::RemovedWorkspaces).await?;
        ctx.show(&removed, |removed| print_removed(removed))?;
        return Ok(0);
    };
    if let Some(checkpoint) = checkpoint {
        return restore_checkpoint(ctx, workspace, checkpoint).await;
    }
    if let Some(checkpoints) = checkpoints(ctx, &workspace).await? {
        ctx.show(&checkpoints, |checkpoints| print_checkpoints(checkpoints))?;
        return Ok(0);
    }
    ensure!(
        !scoped,
        "workspace processes cannot reopen removed workspaces; ask the user to run shoal restore"
    );
    reopen(ctx, workspace).await
}

/// Every checkpoint of an existing workspace, requested page by page;
/// `None` when no workspace matches.
async fn checkpoints(ctx: &Context, workspace: &str) -> Result<Option<Vec<Checkpoint>>> {
    let mut checkpoints = Vec::new();
    loop {
        let after = checkpoints
            .last()
            .map(|checkpoint: &Checkpoint| checkpoint.number);
        let method = Method::Checkpoints {
            workspace: workspace.to_owned(),
            after,
        };
        let Some(page) = request::<Option<Vec<Checkpoint>>>(&ctx.paths, method).await? else {
            return Ok(None);
        };
        if page.is_empty() {
            return Ok(Some(checkpoints));
        }
        checkpoints.extend(page);
    }
}

async fn restore_checkpoint(ctx: &Context, workspace: String, checkpoint: u64) -> Result<i32> {
    let restored: RestoredCheckpoint = ctx
        .progress(
            "Restoring checkpoint",
            request(
                &ctx.paths,
                Method::RestoreCheckpoint {
                    workspace,
                    checkpoint,
                },
            ),
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

/// Open a removed workspace again from its removal record.
async fn reopen(ctx: &Context, workspace: String) -> Result<i32> {
    let restored = ctx
        .progress(
            "Restoring workspace",
            request::<RestoredWorkspace>(&ctx.paths, Method::RestoreWorkspace { workspace }),
        )
        .await?;
    let RestoredWorkspace {
        opened,
        removed,
        warnings,
    } = restored;
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    let mut workspace = opened.workspace;
    if workspace.state == WorkspaceState::Preparing {
        let Some(ready) = super::workspaces::setup_workspace(ctx, &workspace).await? else {
            return Ok(1);
        };
        workspace = ready;
    }
    for url in &removed.links.prs {
        workspace = super::workspaces::link_pull(ctx, workspace, url.clone()).await?;
    }
    let files = if removed.checkpoint.commit != removed.checkpoint.head {
        "; unsaved files restored"
    } else if removed.checkpoint.files_omitted {
        "; unsaved files were not kept"
    } else {
        ""
    };
    let agents = match removed.agents.len() {
        0 => String::new(),
        1 => format!(
            "; shoal resume {} continues its stopped agent",
            workspace.name
        ),
        _ => format!(
            "; shoal resume {} continues its stopped agents",
            workspace.name
        ),
    };
    ctx.emit(
        &format!(
            "Restored {} on branch {} at {}{files}{agents}",
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            workspace.branch,
            workspace.path.display()
        ),
        json!({"workspace": &workspace, "removed": &removed, "warnings": warnings}),
    )?;
    if terminal::follows_caller(ctx) {
        shell::navigate(&workspace.path, ctx.json)?;
    }
    super::workspaces::run_post_setup(ctx, &workspace).await?;
    Ok(0)
}

fn print_removed(removed: &[RemovedWorkspace]) {
    if removed.is_empty() {
        println!("No removed workspaces to restore");
        return;
    }
    let width = removed
        .iter()
        .map(|workspace| workspace.name.chars().count())
        .max()
        .unwrap_or(0);
    for workspace in removed {
        let branch = branch_label(workspace.checkpoint.branch.as_deref());
        println!(
            "{:width$}  {branch}  removed {} ({}), restorable until {}",
            workspace.name,
            local_seconds(workspace.removed_at as i64),
            workspace.cause,
            local_seconds(workspace.expires_at as i64),
        );
    }
}

fn print_checkpoints(checkpoints: &[Checkpoint]) {
    if checkpoints.is_empty() {
        println!("No checkpoints");
        return;
    }
    let width = checkpoints
        .last()
        .map_or(1, |checkpoint| checkpoint.number.to_string().len());
    for checkpoint in checkpoints {
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
        println!(
            "{:>width$}  {}  {}  {}{name}{files}",
            checkpoint.number,
            local_seconds(checkpoint.created_at as i64),
            cause_label(checkpoint.cause),
            branch_label(checkpoint.branch.as_deref()),
        );
    }
}

fn cause_label(cause: CheckpointCause) -> &'static str {
    match cause {
        CheckpointCause::Manual => "manual",
        CheckpointCause::AgentStopped => "agent stopped",
        CheckpointCause::Restore => "before restore",
        CheckpointCause::Removal => "removal",
    }
}

fn branch_label(branch: Option<&str>) -> &str {
    branch.unwrap_or("detached HEAD")
}
