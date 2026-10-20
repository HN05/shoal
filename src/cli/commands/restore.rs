//! Opening removed workspaces again from their restore records.
use anyhow::{Result, ensure};
use serde_json::json;

use crate::{
    cli::{
        client::request,
        context::Context,
        output::{Palette, Style},
        terminal,
    },
    model::{RemovedWorkspace, RestoredWorkspace},
    protocol::Method,
    shell,
    state::WorkspaceState,
    time::local_seconds,
};

/// `shoal restore`: the named workspace, or the list of restorable ones.
pub(super) async fn run(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot restore workspaces; ask the user to run shoal restore"
    );
    let Some(workspace) = workspace else {
        let removed =
            request::<Vec<RemovedWorkspace>>(&ctx.paths, Method::RemovedWorkspaces).await?;
        ctx.show(&removed, |removed| print_removed(removed))?;
        return Ok(0);
    };
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
    let files = if removed.snapshot != removed.head {
        "; unsaved files restored"
    } else if removed.files_omitted {
        "; unsaved files were not kept"
    } else {
        ""
    };
    ctx.emit(
        &format!(
            "Restored {} on branch {} at {}{files}",
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
        let branch = workspace.branch.as_deref().unwrap_or("detached HEAD");
        println!(
            "{:width$}  {branch}  removed {} ({}), restorable until {}",
            workspace.name,
            local_seconds(workspace.removed_at as i64),
            workspace.cause,
            local_seconds(workspace.expires_at as i64),
        );
    }
}
