//! `shoal files` lists the files attached to a workspace; `add` attaches
//! more without a message and `rm` deletes them.
use std::path::PathBuf;

use anyhow::{Result, ensure};

use crate::{
    cli::{
        args::WorkspaceScope,
        client,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::workspace_files::{AttachedFile, FileSource},
    protocol::FileMethod,
};

/// Print the workspace's files, or with `paths` only where Shoal keeps them.
pub(super) async fn list(ctx: &Context, workspace: Option<String>, paths: bool) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let files =
        client::pages::<AttachedFile>(&ctx.paths, FileMethod::ListFiles { workspace }).await?;
    ctx.show(&files, |files| {
        if paths {
            for file in files {
                outln!("{}", file.path.display());
            }
            return;
        }
        let now = crate::time::unix_seconds() as i64;
        let rows = files.iter().map(|file| cells(file, now)).collect();
        for line in ui::aligned(rows, Palette::stdout(ctx.json)) {
            outln!("{line}");
        }
        if files.is_empty() {
            outln!("No files");
        }
    })?;
    Ok(0)
}

/// Attach copies of `files` to the workspace, checking each before a
/// workspace is picked.
pub(super) async fn add(
    ctx: &Context,
    workspace: Option<String>,
    files: Vec<PathBuf>,
) -> Result<i32> {
    let files = super::messages::attachment_paths(&files)?;
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let added =
        client::pages::<AttachedFile>(&ctx.paths, FileMethod::AddFiles { workspace, files })
            .await?;
    ctx.show(&added, |added| {
        let now = crate::time::unix_seconds() as i64;
        let rows = added.iter().map(|file| cells(file, now)).collect();
        for line in ui::aligned(rows, Palette::stdout(ctx.json)) {
            outln!("{line}");
        }
    })?;
    Ok(0)
}

/// Delete the files of the workspace `scope` selects, only those with one of
/// `names` when there are any, or of every workspace without a running agent
/// or command. Asks first for every workspace, or one where an agent or command
/// runs, unless `yes`.
pub(super) async fn remove(
    ctx: &Context,
    scope: WorkspaceScope,
    names: Vec<String>,
    yes: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace_filter(ctx, scope).await?;
    if !yes && let Some(action) = confirmation(ctx, workspace.as_deref()).await? {
        ensure!(ui::confirm(ctx, &action, "--yes")?, "deletion canceled");
    }
    let deleted =
        client::pages::<AttachedFile>(&ctx.paths, FileMethod::RemoveFiles { workspace, names })
            .await?;
    let text = match deleted.len() {
        0 => "No files deleted".to_owned(),
        1 => "Deleted 1 file".to_owned(),
        n => format!("Deleted {n} files"),
    };
    ctx.emit(&text, &deleted)?;
    Ok(0)
}

/// What deleting the files of `workspace`, or of every workspace, asks the
/// user to confirm, if anything.
async fn confirmation(ctx: &Context, workspace: Option<&str>) -> Result<Option<String>> {
    let Some(workspace) = workspace else {
        return Ok(Some(
            "Delete the attached files of every workspace without a running agent or command"
                .into(),
        ));
    };
    let workspaces = client::workspaces(&ctx.paths).await?;
    Ok(workspaces
        .iter()
        .find(|listed| listed.id == workspace || listed.name == workspace)
        .filter(|listed| listed.running)
        .map(|listed| {
            format!(
                "An agent or command runs in {}. Delete its attached files",
                listed.name
            )
        }))
}

/// The file's name, size, how it came, when, read at `now`, when a removed
/// workspace's file is deleted, and Shoal's copy.
fn cells(file: &AttachedFile, now: i64) -> [ui::Cell; 6] {
    let expiry = file
        .expires_at
        .map(|expires_at| format!("until {}", crate::time::local_minutes(expires_at)));
    [
        (file.name.clone(), Some(Style::Heading)),
        (ui::size_label(file.bytes), None),
        (source_label(file.source).into(), None),
        (
            crate::time::local_ago(file.created_at, now),
            Some(Style::Muted),
        ),
        (expiry.unwrap_or_default(), Some(Style::Warning)),
        (file.path.display().to_string(), Some(Style::Muted)),
    ]
}

fn source_label(source: FileSource) -> &'static str {
    match source {
        FileSource::Message => "message",
        FileSource::Added => "added",
        FileSource::Result => "result",
    }
}
