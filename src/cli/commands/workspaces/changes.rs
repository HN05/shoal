//! A workspace's diff and conflicts against its base.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        ui::{self, Fallback},
    },
    execution, git,
    model::{ConflictCheck, DiffBase},
    protocol::WorkspaceMethod,
};

pub(in crate::cli::commands) async fn diff(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let base = request::<DiffBase>(&ctx.paths, WorkspaceMethod::DiffBase { workspace }).await?;
    let command = git::diff_command(&base.commit);
    if crate::env::in_chat().is_some() {
        return diff_untracked(ctx, &base.workspace_id, &command).await;
    }
    execution::run(&ctx.paths, base.workspace_id, command, None).await
}

/// Run the diff `command` in the worktree of `workspace` as a plain process,
/// for a chat, which reads a workspace without running anything there as
/// the workspace's: no tracked execution counts it as the workspace's use.
async fn diff_untracked(ctx: &Context, workspace: &str, command: &[OsString]) -> Result<i32> {
    let workspaces = client::workspaces(&ctx.paths).await?;
    let workspace = (workspaces.iter())
        .find(|candidate| candidate.id == workspace)
        .with_context(|| format!("unknown workspace: {workspace}"))?;
    let status = std::process::Command::new(&command[0])
        .args(&command[1..])
        .current_dir(&workspace.path)
        .status()
        .context("run git diff")?;
    Ok(execution::exit_code(status))
}

pub(in crate::cli::commands) async fn conflicts(
    ctx: &Context,
    workspace: Option<String>,
    target: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let check =
        request::<ConflictCheck>(&ctx.paths, WorkspaceMethod::Conflicts { workspace, target })
            .await?;
    ctx.show(&check, |check| {
        if check.clean() {
            outln!("No conflicts with {}", check.target);
        }
        show_files(&format!("Conflicts with {}", check.target), &check.files);
        show_files(
            &format!("Changed on both sides with {}", check.target),
            &check.changed_on_both_sides,
        );
    })?;
    Ok(i32::from(!check.clean()))
}

/// `files` under `heading`, unless there are none.
fn show_files(heading: &str, files: &[String]) {
    if files.is_empty() {
        return;
    }
    outln!("{heading}:");
    for file in files {
        outln!("  {file}");
    }
}
