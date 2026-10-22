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
    model::RestoredWorkspace,
    protocol::Method,
    removal::{RemovalCheck, UnsavedWork},
    transfer::{ExportedWorkspace, Import},
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
    let output = Output::reserve(&file)?;
    let exported = export_workspace(ctx, workspace, yes, &output).await?;
    output.report(ctx, &exported)
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
    let import = read_import(&file)?;
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

/// Read the export in `file`, or on standard input for `-`.
fn read_import(file: &Path) -> Result<Import> {
    if file.as_os_str() == STANDARD_STREAM {
        return Import::read(&mut std::io::stdin().lock());
    }
    let input = std::fs::File::open(file).with_context(|| format!("open {}", file.display()))?;
    Import::read(&mut BufReader::new(input))
}

/// Where an export goes: a new file whose directory takes a file before
/// the workspace is removed, so an unwritable destination fails first, or a
/// temporary file copied to standard output.
enum Output {
    Stream { directory: tempfile::TempDir },
    File { path: PathBuf },
}

impl Output {
    fn reserve(file: &Path) -> Result<Self> {
        if file.as_os_str() == STANDARD_STREAM {
            return Ok(Self::Stream {
                directory: tempfile::tempdir()?,
            });
        }
        let name = file
            .file_name()
            .with_context(|| format!("{} names no file", file.display()))?;
        let directory = match file.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let directory = std::fs::canonicalize(directory)
            .with_context(|| format!("find the directory {}", directory.display()))?;
        let path = directory.join(name);
        ensure!(
            !path.exists(),
            "{} exists; choose another file",
            path.display()
        );
        tempfile::NamedTempFile::new_in(&directory)
            .with_context(|| format!("create a file in {}", directory.display()))?;
        Ok(Self::File { path })
    }

    /// The file the daemon writes.
    fn file(&self) -> PathBuf {
        match self {
            Self::Stream { directory } => directory.path().join("workspace.shoal"),
            Self::File { path } => path.clone(),
        }
    }

    /// The file the user named; `None` for standard output.
    fn destination(&self) -> Option<&Path> {
        match self {
            Self::Stream { .. } => None,
            Self::File { path } => Some(path),
        }
    }

    /// Report the export, copying it to standard output when it goes there.
    fn report(self, ctx: &Context, exported: &ExportedWorkspace) -> Result<i32> {
        let name = &exported.manifest.workspace.name;
        let Self::File { path } = &self else {
            let mut stdout = std::io::stdout().lock();
            std::io::copy(&mut std::fs::File::open(self.file())?, &mut stdout)?;
            stdout.flush()?;
            eprintln!("Exported {name}");
            return Ok(0);
        };
        ctx.emit(
            &format!(
                "Exported {name} to {}; shoal import opens it on another machine",
                path.display()
            ),
            json!({"file": path, "manifest": exported.manifest, "removal": exported.removal}),
        )?;
        Ok(0)
    }
}

/// Export the workspace to `output` once its checkpoint can keep every
/// unsaved file, `output` lies outside it and the user lets its holds go;
/// a removed workspace with that name or ID is exported from its record.
async fn export_workspace(
    ctx: &Context,
    workspace: Option<String>,
    yes: bool,
    output: &Output,
) -> Result<ExportedWorkspace> {
    let method = |workspace, unsaved| Method::ExportWorkspace {
        workspace,
        unsaved,
        caller_pid: std::process::id(),
        file: output.file(),
    };
    if let Some(selector) = &workspace
        && !workspace_exists(ctx, selector).await?
    {
        let method = method(selector.clone(), UnsavedWork::default());
        return request::<ExportedWorkspace>(&ctx.paths, method).await;
    }
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let check = check_export(ctx, &workspace, yes, output.destination()).await?;
    let method = method(workspace, UnsavedWork::confirming(&check));
    let exported = super::workspaces::leaving(
        ctx,
        &check.workspace,
        request::<ExportedWorkspace>(&ctx.paths, method),
    )
    .await?;
    if let Some(error) = exported
        .removal
        .as_ref()
        .and_then(|removal| removal.hook_error.as_ref())
    {
        eprintln!("warning: {error}");
    }
    Ok(exported)
}

async fn workspace_exists(ctx: &Context, selector: &str) -> Result<bool> {
    Ok(client::workspaces(&ctx.paths)
        .await?
        .iter()
        .any(|workspace| workspace.id == selector || workspace.name == selector))
}

/// What removing the workspace for an export would delete, refusing unsaved
/// files a checkpoint cannot keep and a destination inside it, after the
/// user lets its holds go.
async fn check_export(
    ctx: &Context,
    workspace: &str,
    yes: bool,
    destination: Option<&Path>,
) -> Result<RemovalCheck> {
    let check = request::<RemovalCheck>(
        &ctx.paths,
        Method::CheckRemoval {
            workspace: workspace.to_owned(),
            caller_pid: std::process::id(),
            include_changes: true,
        },
    )
    .await?;
    ensure!(
        !check.dirty || check.unsaved_files_archived,
        "a checkpoint cannot keep the uncommitted files (they are too large, or include a repository or a changed submodule); commit or remove them before exporting"
    );
    if let Some(destination) = destination {
        ensure!(
            !check.workspace.contains(destination),
            "{} is inside the workspace the export removes; write it elsewhere",
            destination.display()
        );
    }
    if !yes && !check.workspace.holds.is_empty() {
        confirm_holds(ctx, &check)?;
    }
    Ok(check)
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
