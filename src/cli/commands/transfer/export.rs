//! `shoal move export`: remove workspaces and write each to a file, or to
//! standard output.
use std::{
    io::Write,
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
    model::Workspace,
    protocol::RemovalMethod,
    removal::{RemovalCheck, UnsavedWork},
    transfer::ExportedWorkspace,
};

use super::STANDARD_STREAM;

/// `shoal export`: remove the workspace, or take an already removed one, and
/// write it to `file`. Several workspaces, or any written to a directory,
/// go to a file each in it. The destination is checked before a picker
/// opens.
pub(in crate::cli::commands) async fn export(
    ctx: &Context,
    names: Vec<String>,
    file: PathBuf,
    yes: bool,
) -> Result<i32> {
    ensure_unscoped_export(ctx)?;
    if let [selector] = names.as_slice()
        && !workspace_exists(ctx, selector).await?
    {
        let output = Output::reserve(&file)?;
        let exported = export_removed(ctx, selector, &output).await?;
        return output.report(ctx, &exported);
    }
    if names.len() > 1 || file.is_dir() {
        ensure!(
            file != Path::new(STANDARD_STREAM),
            "several workspaces export to a file each; name a directory with -o"
        );
        let mut workspaces = Vec::new();
        for selector in ui::select_workspaces(ctx, names).await? {
            workspaces.push(client::inspect(&ctx.paths, selector).await?.workspace);
        }
        return export_into(ctx, &workspaces, file, yes).await;
    }
    let output = Output::reserve(&file)?;
    let named = names.into_iter().next();
    let workspace = ui::select_workspace(ctx, named, Fallback::CurrentDirectory).await?;
    let exported = export_workspace(ctx, &workspace, yes, &output).await?;
    output.report(ctx, &exported)
}

/// `shoal export --all`: export every workspace of `repository` to a file of
/// its own in `directory`.
pub(in crate::cli::commands) async fn export_all(
    ctx: &Context,
    repository: String,
    directory: PathBuf,
    yes: bool,
) -> Result<i32> {
    ensure_unscoped_export(ctx)?;
    let workspaces = repository_workspaces(ctx, repository).await?;
    export_into(ctx, &workspaces, directory, yes).await
}

/// Export `workspaces` to a file each in `directory`, once each one can be
/// exported, so a workspace that cannot be leaves every other one in place.
async fn export_into(
    ctx: &Context,
    workspaces: &[Workspace],
    directory: PathBuf,
    yes: bool,
) -> Result<i32> {
    let directory = ExportDirectory::create(directory)?;
    let planned = check_exports(ctx, workspaces, &directory, yes).await?;
    export_each(ctx, planned, &directory).await
}

fn ensure_unscoped_export(ctx: &Context) -> Result<()> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot export workspaces; ask the user to run shoal export"
    );
    Ok(())
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
            let mut file = std::fs::File::open(self.file())?;
            crate::stdout::write(|stdout| {
                std::io::copy(&mut file, stdout)?;
                stdout.flush()
            })?;
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

/// Export the removed workspace `selector` names, which needs no removal.
async fn export_removed(
    ctx: &Context,
    selector: &str,
    output: &Output,
) -> Result<ExportedWorkspace> {
    let method = RemovalMethod::ExportWorkspace {
        workspace: selector.to_owned(),
        unsaved: UnsavedWork::default(),
        file: output.file(),
    };
    request::<ExportedWorkspace>(&ctx.paths, method).await
}

/// Export the workspace to `output` once its checkpoint can keep every
/// unsaved file, `output` lies outside it and the user lets its holds go;
/// a removed workspace with that name or ID is exported from its record.
async fn export_workspace(
    ctx: &Context,
    workspace: &str,
    yes: bool,
    output: &Output,
) -> Result<ExportedWorkspace> {
    let check = check_export(ctx, workspace, output).await?;
    ensure_children_exported(&check, &[])?;
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
    let method = RemovalMethod::ExportWorkspace {
        workspace: check.workspace.id.clone(),
        unsaved: UnsavedWork::confirming(check),
        file: output.file(),
    };
    let exported = super::super::workspaces::leaving(
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
        RemovalMethod::CheckRemoval {
            workspace: workspace.to_owned(),
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
            !crate::cli::terminal::closes_with(&ctx.paths, &check.workspace.id).await?,
            "this terminal closes when the export removes the workspace, ending the stream; export to a file with -o, or from another terminal"
        ),
    }
    Ok(check)
}

/// Refuse to export the workspace `check` describes without the workspaces
/// below it, which removing it here would take unexported, unless they are
/// among the `exported` too.
fn ensure_children_exported(check: &RemovalCheck, exported: &[&str]) -> Result<()> {
    let missing: Vec<&str> = check
        .children
        .iter()
        .map(|child| child.name.as_str())
        .filter(|child| !exported.contains(child))
        .collect();
    ensure!(
        missing.is_empty(),
        "workspaces were split from it ({}); export them with it, as shoal export --all does, or remove them first",
        missing.join(", ")
    );
    Ok(())
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
    let exported: Vec<&str> = workspaces.iter().map(|w| w.name.as_str()).collect();
    for workspace in workspaces {
        let export = async {
            let export = plan_export(ctx, workspace, directory).await?;
            ensure_children_exported(&export.check, &exported)?;
            Ok::<_, anyhow::Error>(export)
        };
        match export.await {
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

/// Reserve the workspace's file in `directory` and check its removal as a
/// batch's.
async fn plan_export(
    ctx: &Context,
    workspace: &Workspace,
    directory: &ExportDirectory,
) -> Result<PlannedExport> {
    let output = Output::reserve(&directory.file(&workspace.name))?;
    let check = check_export(ctx, &workspace.id, &output).await?;
    super::super::workspaces::ensure_batch_removable(ctx, &check, ("export", "export")).await?;
    Ok(PlannedExport { check, output })
}

/// Export each planned workspace, going on after one fails.
async fn export_each(
    ctx: &Context,
    planned: Vec<PlannedExport>,
    directory: &ExportDirectory,
) -> Result<i32> {
    let mut report = ui::Report::new("exported", "export");
    for PlannedExport { check, output } in planned {
        let name = &check.workspace.name;
        match export_checked(ctx, &check, &output).await {
            Ok(exported) => {
                let message = format!(
                    "Exported {name} to {}",
                    directory.shown_file(name).display()
                );
                let value = json!({
                    "file": directory.file(name),
                    "manifest": exported.manifest,
                    "removal": exported.removal,
                });
                report.done(ctx, &message, value);
            }
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    let (exported, failed) = report.counts();
    let shown = directory.shown.display();
    let summary = if failed == 0 {
        format!(
            "Exported {exported} workspaces to {shown}; shoal import {shown} opens them on another machine"
        )
    } else {
        format!(
            "Exported {exported} of {} workspaces to {shown}; {failed} failed",
            exported + failed
        )
    };
    report.field("directory", json!(directory.path));
    report.finish(ctx, &summary)
}
