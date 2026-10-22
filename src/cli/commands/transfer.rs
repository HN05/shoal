//! Moving workspaces to another Shoal: `shoal export` removes a workspace and
//! writes it to a file, and `shoal import` opens it again from one.
use std::{
    io::BufReader,
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
    model::{RemovedWorkspace, Repository, RestoredWorkspace},
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
    let output = Output::reserve(&file)?;
    let removed = removed_for_export(ctx, workspace, yes, output.path()).await?;
    let repo = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == removed.repository_id)
        .context("the workspace's repository is no longer registered")?;
    let name = removed.name.clone();
    match output.write(&repo, removed).await? {
        (None, _) => eprintln!("Exported {name}"),
        (Some(path), manifest) => ctx.emit(
            &format!(
                "Exported {name} to {}; shoal import opens it on another machine",
                path.display()
            ),
            json!({"file": path, "manifest": manifest}),
        )?,
    }
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

/// Where an export goes: standard output, or a new file whose directory
/// takes a temporary file before the workspace is removed, so an unwritable
/// destination fails first and a failed export leaves no partial file.
enum Output {
    Stream,
    File {
        path: PathBuf,
        file: tempfile::NamedTempFile,
    },
}

impl Output {
    fn reserve(file: &Path) -> Result<Self> {
        if file.as_os_str() == STANDARD_STREAM {
            return Ok(Self::Stream);
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
        let file = tempfile::NamedTempFile::new_in(&directory)
            .with_context(|| format!("create a file in {}", directory.display()))?;
        Ok(Self::File { path, file })
    }

    /// The file's canonical path; `None` for standard output.
    fn path(&self) -> Option<&Path> {
        match self {
            Self::Stream => None,
            Self::File { path, .. } => Some(path),
        }
    }

    /// Export `removed` here, returning the file's path with the manifest.
    async fn write(
        self,
        repo: &Repository,
        removed: RemovedWorkspace,
    ) -> Result<(Option<PathBuf>, transfer::Manifest)> {
        let (path, file) = match self {
            Self::Stream => {
                let mut out = std::io::BufWriter::new(std::io::stdout());
                return Ok((None, transfer::export(repo, removed, &mut out).await?));
            }
            Self::File { path, file } => (path, file),
        };
        let mut out = std::io::BufWriter::new(file.as_file());
        let manifest = transfer::export(repo, removed, &mut out).await?;
        out.into_inner().map_err(|error| error.into_error())?;
        file.as_file().sync_all()?;
        file.persist_noclobber(&path)
            .with_context(|| format!("write {}", path.display()))?;
        Ok((Some(path), manifest))
    }
}

/// The removal record to export: the workspace's, removed now, when it
/// exists and does not hold `output`; otherwise the latest of a removed
/// workspace with that name or ID.
async fn removed_for_export(
    ctx: &Context,
    workspace: Option<String>,
    yes: bool,
    output: Option<&Path>,
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
    let id = remove_for_export(ctx, workspace, yes, output).await?;
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

/// Remove the workspace once its checkpoint can keep every unsaved file, it
/// does not hold `output`, and the user lets its holds go; its ID names the
/// removal record.
async fn remove_for_export(
    ctx: &Context,
    workspace: String,
    yes: bool,
    output: Option<&Path>,
) -> Result<String> {
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
    if let Some(output) = output {
        ensure!(
            !check.workspace.contains(output),
            "{} is inside the workspace the export removes; write it elsewhere",
            output.display()
        );
    }
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
