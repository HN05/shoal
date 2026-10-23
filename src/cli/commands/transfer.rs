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
        agents,
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    model::{RemovedWorkspace, RestoredWorkspace, Workspace},
    protocol::Method,
    removal::{RemovalCheck, UnsavedWork},
    state::WorkspaceState,
    transfer::{ExportedWorkspace, Import, Manifest},
};

use super::restore::Reopened;

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
    ensure_unscoped_export(ctx)?;
    let output = Output::reserve(&file)?;
    let exported = export_workspace(ctx, workspace, yes, &output).await?;
    output.report(ctx, &exported)
}

/// `shoal export --all`: export every workspace of `repository` to a file of
/// its own in `directory`, once each one can be exported, so a workspace that
/// cannot be leaves every other one in place.
pub(super) async fn export_all(
    ctx: &Context,
    repository: String,
    directory: PathBuf,
    yes: bool,
) -> Result<i32> {
    ensure_unscoped_export(ctx)?;
    let workspaces = repository_workspaces(ctx, repository).await?;
    let directory = ExportDirectory::create(directory)?;
    let planned = check_exports(ctx, &workspaces, &directory, yes).await?;
    export_each(ctx, planned, &directory)
        .await
        .report(ctx, &directory)
}

fn ensure_unscoped_export(ctx: &Context) -> Result<()> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot export workspaces; ask the user to run shoal export"
    );
    Ok(())
}

/// `shoal import`: open the workspaces exported to `files`, or to the
/// `.shoal` files in the directories among them, in the repository with each
/// one's origin, or in `repository`. A directory or several files import
/// each workspace and report every result.
pub(super) async fn import(
    ctx: &Context,
    files: Vec<PathBuf>,
    repository: Option<String>,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot import workspaces; ask the user to run shoal import"
    );
    if let [file] = files.as_slice()
        && !file.is_dir()
    {
        let restored = import_workspace(ctx, file, repository).await?;
        return super::restore::finish_reopen(ctx, restored, "Imported").await;
    }
    let files = import_files(files)?;
    import_each(ctx, &files, repository).await.report(ctx)
}

/// Record the workspace exported to `file` as removed in its repository, or
/// in `repository`, and open it again there.
async fn import_workspace(
    ctx: &Context,
    file: &Path,
    repository: Option<String>,
) -> Result<RestoredWorkspace> {
    let import = read_import(file)?;
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
        workspace: Box::new(workspace_here(ctx, &import.manifest)),
    };
    ctx.progress(
        "Importing workspace",
        request::<RestoredWorkspace>(&ctx.paths, method),
    )
    .await
}

/// The export files `files` names, with each directory among them replaced
/// by its `.shoal` files in name order.
fn import_files(files: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for file in files {
        ensure!(
            file.as_os_str() != STANDARD_STREAM,
            "standard input holds one export; import - on its own"
        );
        if !file.is_dir() {
            found.push(file);
            continue;
        }
        let mut exports = Vec::new();
        for entry in std::fs::read_dir(&file).with_context(|| format!("read {}", file.display()))? {
            let path = entry?.path();
            if path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "shoal")
            {
                exports.push(path);
            }
        }
        ensure!(
            !exports.is_empty(),
            "{} holds no .shoal files",
            file.display()
        );
        exports.sort();
        found.extend(exports);
    }
    Ok(found)
}

/// Import each of `files`, going on after one fails.
async fn import_each(ctx: &Context, files: &[PathBuf], repository: Option<String>) -> ImportReport {
    let mut report = ImportReport::default();
    for file in files {
        match import_and_prepare(ctx, file, repository.clone()).await {
            Ok(reopened) => {
                let workspace = reopened.workspace.clone();
                report.imported(ctx, reopened);
                if let Err(error) = super::workspaces::run_post_setup(ctx, &workspace).await {
                    let error = error.context(format!(
                        "imported {}, but its post_setup_cmd failed",
                        workspace.name
                    ));
                    report.failed(ctx, file, &error);
                }
            }
            Err(error) => report.failed(ctx, file, &error),
        }
    }
    report
}

/// Import the workspace exported to `file` and set it up.
async fn import_and_prepare(
    ctx: &Context,
    file: &Path,
    repository: Option<String>,
) -> Result<Reopened> {
    let restored = import_workspace(ctx, file, repository).await?;
    super::restore::prepare_reopened(ctx, restored)
        .await?
        .context("its setup did not finish")
}

/// The workspaces an import of several files opened, and the files it could
/// not import.
#[derive(Default)]
struct ImportReport {
    imported: Vec<Reopened>,
    failed: Vec<serde_json::Value>,
}

impl ImportReport {
    fn imported(&mut self, ctx: &Context, reopened: Reopened) {
        if !ctx.json {
            println!("{}", reopened.message(ctx, "Imported"));
        }
        self.imported.push(reopened);
    }

    fn failed(&mut self, ctx: &Context, file: &Path, error: &anyhow::Error) {
        eprintln!(
            "{} cannot import {}: {error:#}",
            Palette::stderr(ctx.json).paint(Style::Error, "error:"),
            file.display()
        );
        self.failed
            .push(json!({"file": file, "error": format!("{error:#}")}));
    }

    fn report(self, ctx: &Context) -> Result<i32> {
        let imported = self.imported.len();
        let message = if self.failed.is_empty() {
            format!("Imported {imported} workspaces")
        } else {
            format!(
                "Imported {imported} workspaces; {} failed",
                self.failed.len()
            )
        };
        ctx.emit(
            &message,
            json!({"imported": self.imported, "failed": self.failed}),
        )?;
        Ok(if self.failed.is_empty() { 0 } else { 1 })
    }
}

/// The exported removal record for this machine: on the one that exported
/// it, agents keep the sessions still there.
fn workspace_here(ctx: &Context, manifest: &Manifest) -> RemovedWorkspace {
    let machine = crate::process::machine::id();
    manifest.workspace_on(machine.as_deref(), |agent, session| {
        agents::has_session(ctx, agent, session)
    })
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
    if let Some(selector) = &workspace
        && !workspace_exists(ctx, selector).await?
    {
        let method = Method::ExportWorkspace {
            workspace: selector.clone(),
            unsaved: UnsavedWork::default(),
            caller_pid: std::process::id(),
            file: output.file(),
        };
        return request::<ExportedWorkspace>(&ctx.paths, method).await;
    }
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let check = check_export(ctx, &workspace, output).await?;
    let action = format!(
        "Export workspace: {}\nRemoves it here and releases its holds:",
        check.workspace.name
    );
    confirm_holds(ctx, yes, action, &[&check])?;
    export_checked(ctx, &check, output).await
}

/// Remove the workspace `check` describes and export it to `output`.
async fn export_checked(
    ctx: &Context,
    check: &RemovalCheck,
    output: &Output,
) -> Result<ExportedWorkspace> {
    let method = Method::ExportWorkspace {
        workspace: check.workspace.id.clone(),
        unsaved: UnsavedWork::confirming(check),
        caller_pid: std::process::id(),
        file: output.file(),
    };
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
/// files a checkpoint cannot keep and an output the removal would cut off.
async fn check_export(ctx: &Context, workspace: &str, output: &Output) -> Result<RemovalCheck> {
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
    match output.destination() {
        Some(destination) => ensure!(
            !check.workspace.contains(destination),
            "{} is inside the workspace the export removes; write it elsewhere",
            destination.display()
        ),
        None => ensure!(
            !crate::cli::terminal::closes_with(&ctx.paths, &check.workspace.id),
            "this tab closes when the export removes the workspace, ending the stream; export to a file with -o, or from another tab"
        ),
    }
    Ok(check)
}

/// Ask with `action`, unless `yes` confirms it, before releasing the holds
/// external sessions keep on the workspaces `checks` describe, naming each
/// hold's workspace when there are several.
fn confirm_holds(
    ctx: &Context,
    yes: bool,
    mut action: String,
    checks: &[&RemovalCheck],
) -> Result<()> {
    if yes || checks.iter().all(|check| check.workspace.holds.is_empty()) {
        return Ok(());
    }
    for check in checks {
        for hold in &check.workspace.holds {
            action.push_str("\n  ");
            if checks.len() > 1 {
                action.push_str(&format!("{}: ", check.workspace.name));
            }
            action.push_str(&hold.name);
            if let Some(reason) = &hold.reason {
                action.push_str(&format!(" ({reason})"));
            }
        }
    }
    ensure!(
        ui::confirm(ctx, &action, "--yes")?,
        "workspace export canceled"
    );
    Ok(())
}

/// The workspaces of `repository`, by name.
async fn repository_workspaces(ctx: &Context, repository: String) -> Result<Vec<Workspace>> {
    let repositories = client::repositories(&ctx.paths).await?;
    let repo =
        crate::repository::select(&repositories, ui::repository_selector(repository)?).await?;
    let mut workspaces: Vec<Workspace> = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .filter(|workspace| workspace.repository_id == repo.id)
        .collect();
    ensure!(
        !workspaces.is_empty(),
        "{} has no workspaces to export",
        crate::repository::name(repo)
    );
    workspaces.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(workspaces)
}

/// The directory an export of every workspace goes to, shown as the user
/// named it.
struct ExportDirectory {
    path: PathBuf,
    shown: PathBuf,
}

impl ExportDirectory {
    /// Create the directory when it is missing.
    fn create(shown: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&shown)
            .with_context(|| format!("create the directory {}", shown.display()))?;
        let path = std::fs::canonicalize(&shown)
            .with_context(|| format!("find the directory {}", shown.display()))?;
        Ok(Self { path, shown })
    }

    /// The file `workspace` goes to.
    fn file(&self, workspace: &str) -> PathBuf {
        self.path.join(Self::file_name(workspace))
    }

    /// The file `workspace` goes to, as the user names it.
    fn shown_file(&self, workspace: &str) -> PathBuf {
        self.shown.join(Self::file_name(workspace))
    }

    fn file_name(workspace: &str) -> String {
        format!("{workspace}.shoal")
    }
}

/// A workspace that passed its export check, with the file it goes to.
struct PlannedExport {
    check: RemovalCheck,
    output: Output,
}

/// Check every workspace before exporting any, refusing with each one that
/// cannot be exported, then let the user release all of their holds at once.
async fn check_exports(
    ctx: &Context,
    workspaces: &[Workspace],
    directory: &ExportDirectory,
    yes: bool,
) -> Result<Vec<PlannedExport>> {
    let mut planned = Vec::new();
    let mut refusals = Vec::new();
    for workspace in workspaces {
        match plan_export(ctx, workspace, directory).await {
            Ok(export) => planned.push(export),
            Err(error) => refusals.push(format!("{}: {error:#}", workspace.name)),
        }
    }
    ensure!(
        refusals.is_empty(),
        "cannot export these workspaces, so none was removed:\n  {}",
        refusals.join("\n  ")
    );
    let action = format!(
        "Export {} workspaces to {}\nRemoves them here and releases these holds:",
        planned.len(),
        directory.shown.display()
    );
    let held: Vec<&RemovalCheck> = planned
        .iter()
        .map(|export| &export.check)
        .filter(|check| !check.workspace.holds.is_empty())
        .collect();
    confirm_holds(ctx, yes, action, &held)?;
    Ok(planned)
}

/// Reserve the workspace's file in `directory` and check its removal,
/// including the lifecycle state removal accepts, which the removal check
/// leaves to the removal itself. The export goes on from this terminal after
/// each removal, so it must not close with any of the workspaces.
async fn plan_export(
    ctx: &Context,
    workspace: &Workspace,
    directory: &ExportDirectory,
) -> Result<PlannedExport> {
    let output = Output::reserve(&directory.file(&workspace.name))?;
    let check = check_export(ctx, &workspace.id, &output).await?;
    let state = check.workspace.state;
    ensure!(
        matches!(state, WorkspaceState::Ready | WorkspaceState::Failed),
        "workspace is {state}; wait for that operation to finish"
    );
    ensure!(
        !crate::cli::terminal::closes_with(&ctx.paths, &workspace.id),
        "this tab closes when the export removes the workspace, ending the export of the others; export from another tab"
    );
    Ok(PlannedExport { check, output })
}

/// Export each planned workspace, going on after one fails.
async fn export_each(
    ctx: &Context,
    planned: Vec<PlannedExport>,
    directory: &ExportDirectory,
) -> ExportReport {
    let mut report = ExportReport::default();
    for PlannedExport { check, output } in planned {
        let name = &check.workspace.name;
        match export_checked(ctx, &check, &output).await {
            Ok(exported) => report.exported(ctx, name, directory, exported),
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    report
}

/// What an export of every workspace wrote, and the workspaces it could not
/// export.
#[derive(Default)]
struct ExportReport {
    exported: Vec<serde_json::Value>,
    failed: Vec<serde_json::Value>,
}

impl ExportReport {
    fn exported(
        &mut self,
        ctx: &Context,
        name: &str,
        directory: &ExportDirectory,
        exported: ExportedWorkspace,
    ) {
        if !ctx.json {
            println!(
                "Exported {name} to {}",
                directory.shown_file(name).display()
            );
        }
        self.exported.push(json!({
            "file": directory.file(name),
            "manifest": exported.manifest,
            "removal": exported.removal,
        }));
    }

    fn failed(&mut self, ctx: &Context, name: &str, error: &anyhow::Error) {
        eprintln!(
            "{} cannot export {name}: {error:#}",
            Palette::stderr(ctx.json).paint(Style::Error, "error:")
        );
        self.failed
            .push(json!({"workspace": name, "error": format!("{error:#}")}));
    }

    fn report(self, ctx: &Context, directory: &ExportDirectory) -> Result<i32> {
        let exported = self.exported.len();
        let shown = directory.shown.display();
        let message = if self.failed.is_empty() {
            format!(
                "Exported {exported} workspaces to {shown}; shoal import {shown} opens them on another machine"
            )
        } else {
            format!(
                "Exported {exported} of {} workspaces to {shown}; {} failed",
                exported + self.failed.len(),
                self.failed.len()
            )
        };
        ctx.emit(
            &message,
            json!({"directory": directory.path, "exported": self.exported, "failed": self.failed}),
        )?;
        Ok(if self.failed.is_empty() { 0 } else { 1 })
    }
}
