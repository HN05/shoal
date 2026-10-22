//! Moving workspaces to another Shoal: `shoal export` removes a workspace and
//! writes it to a file, and `shoal import` opens it again from one.
use std::{
    io::{BufReader, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use serde_json::json;

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        ui::{self, Fallback},
    },
    model::{RemovedWorkspace, RestoredWorkspace},
    protocol::Method,
    removal::RemovalCheck,
    transfer::{self, Import},
};

/// The file name that reads or writes the standard streams.
const STANDARD_STREAM: &str = "-";

/// `shoal export`: remove the workspace, or take an already removed one, and
/// write it to `file`.
pub(super) async fn export(
    ctx: &Context,
    workspace: Option<String>,
    file: PathBuf,
    yes: bool,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot export workspaces; ask the user to run shoal export"
    );
    let stream = file.as_os_str() == STANDARD_STREAM;
    ensure!(
        stream || !file.exists(),
        "{} exists; choose another file",
        file.display()
    );
    let removed = removed_for_export(ctx, workspace, yes).await?;
    let repo = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == removed.repository_id)
        .context("the workspace's repository is no longer registered")?;
    let name = removed.name.clone();
    if stream {
        let mut out = std::io::BufWriter::new(std::io::stdout());
        transfer::export(&repo, removed, &mut out).await?;
        eprintln!("Exported {name}");
        return Ok(0);
    }
    let manifest = write_new(&file, async |out| {
        transfer::export(&repo, removed, out).await
    })
    .await?;
    ctx.emit(
        &format!(
            "Exported {name} to {}; shoal import opens it on another machine",
            file.display()
        ),
        json!({"file": file, "manifest": manifest}),
    )?;
    Ok(0)
}

/// `shoal import`: open the workspace exported to `file` in the repository
/// with its origin, or in `repository`.
pub(super) async fn import(
    ctx: &Context,
    file: PathBuf,
    repository: Option<String>,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot import workspaces; ask the user to run shoal import"
    );
    let import = if file.as_os_str() == STANDARD_STREAM {
        Import::read(&mut std::io::stdin().lock())?
    } else {
        let input =
            std::fs::File::open(&file).with_context(|| format!("open {}", file.display()))?;
        Import::read(&mut BufReader::new(input))?
    };
    let repository = repository
        .or_else(|| import.manifest.remote_url.clone())
        .context("the exported repository has no origin; select the repository to import into with --repo")?;
    let method = Method::ImportWorkspace {
        repository: ui::repository_selector(repository)?,
        bundle: import
            .bundle
            .as_ref()
            .map(|bundle| bundle.path().to_owned()),
        prerequisite: import.manifest.prerequisite.clone(),
        workspace: Box::new(import.manifest.workspace.clone()),
    };
    let restored = ctx
        .progress(
            "Importing workspace",
            request::<RestoredWorkspace>(&ctx.paths, method),
        )
        .await?;
    drop(import);
    super::restore::finish_reopen(ctx, restored, "Imported").await
}

/// The removal record to export: the workspace's, removed now, when it
/// exists; otherwise the latest of a removed workspace with that name or ID.
async fn removed_for_export(
    ctx: &Context,
    workspace: Option<String>,
    yes: bool,
) -> Result<RemovedWorkspace> {
    if let Some(selector) = &workspace
        && !workspace_exists(ctx, selector).await?
    {
        return removed_workspace(ctx, |removed| {
            &removed.workspace_id == selector || &removed.name == selector
        })
        .await?
        .with_context(|| format!("no workspace or restorable workspace {selector}"));
    }
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let id = remove_for_export(ctx, workspace, yes).await?;
    let removed = removed_workspace(ctx, |removed| removed.workspace_id == id)
        .await?
        .context("the removal kept no record to export")?;
    ensure!(
        !removed.checkpoint.files_omitted,
        "the removal could not keep the uncommitted files; shoal restore {} opens the workspace here again",
        removed.name
    );
    Ok(removed)
}

async fn workspace_exists(ctx: &Context, selector: &str) -> Result<bool> {
    Ok(client::workspaces(&ctx.paths)
        .await?
        .iter()
        .any(|workspace| workspace.id == selector || workspace.name == selector))
}

/// The newest restorable workspace `matches` accepts.
async fn removed_workspace(
    ctx: &Context,
    matches: impl Fn(&RemovedWorkspace) -> bool,
) -> Result<Option<RemovedWorkspace>> {
    Ok(
        request::<Vec<RemovedWorkspace>>(&ctx.paths, Method::RemovedWorkspaces)
            .await?
            .into_iter()
            .find(matches),
    )
}

/// Remove the workspace once its checkpoint can keep every unsaved file and
/// the user lets its holds go; its ID names the removal record.
async fn remove_for_export(ctx: &Context, workspace: String, yes: bool) -> Result<String> {
    let check = request::<RemovalCheck>(
        &ctx.paths,
        Method::CheckRemoval {
            workspace: workspace.clone(),
            caller_pid: std::process::id(),
            include_changes: true,
        },
    )
    .await?;
    ensure!(
        !check.dirty || check.unsaved_files_archived,
        "a checkpoint cannot keep the uncommitted files (they are too large, or include a repository or a changed submodule); commit or remove them before exporting"
    );
    if !yes && !check.workspace.holds.is_empty() {
        confirm_holds(ctx, &check)?;
    }
    super::workspaces::remove_checked(ctx, workspace, &check).await?;
    Ok(check.workspace.id)
}

/// Ask before releasing the holds external sessions keep on the workspace.
fn confirm_holds(ctx: &Context, check: &RemovalCheck) -> Result<()> {
    let mut action = format!(
        "Export workspace: {}\nRemoves it here and releases its holds:",
        check.workspace.name
    );
    for hold in &check.workspace.holds {
        action.push_str("\n  ");
        action.push_str(&hold.name);
        if let Some(reason) = &hold.reason {
            action.push_str(&format!(" ({reason})"));
        }
    }
    ensure!(
        ui::confirm(ctx, &action, "--yes")?,
        "workspace export canceled"
    );
    Ok(())
}

/// Write a new file at `path` with `write`, so a failure leaves no partial
/// file and an existing one is never replaced.
async fn write_new<T>(
    path: &Path,
    write: impl AsyncFnOnce(&mut std::io::BufWriter<&std::fs::File>) -> Result<T>,
) -> Result<T> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let file = tempfile::NamedTempFile::new_in(directory)
        .with_context(|| format!("create a file in {}", directory.display()))?;
    let mut out = std::io::BufWriter::new(file.as_file());
    let value = write(&mut out).await?;
    out.flush()?;
    drop(out);
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(value)
}
