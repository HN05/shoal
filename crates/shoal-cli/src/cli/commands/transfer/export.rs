//! `shoal move export`: end spaces here, removing workspaces and stopping
//! chats' agents, and write each to a file, or to standard output.
use crate::transfer::ManifestExt;
use shoal_ids::space_ids::SpaceId;
use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::client::output::transfer::Exported;
use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    model::Space,
    removal::{RemovalResult, UnsavedWork},
    transfer::ExportedSpace,
};
use shoal_protocol::space_names::SpaceSelector;

use super::STANDARD_STREAM;

mod checks;
pub(super) mod copy;
mod directory;

pub(super) use checks::{Checked, Ending};
use checks::{check_export, confirm_ending, exported_alone};
use directory::DirectoryReport;
pub(super) use directory::ExportDirectory;

/// `shoal move export`: end the space here, or take an already removed
/// one, and write it to `file`. Several spaces, or any written to a
/// directory, go to a file each in it. The destination is checked before a
/// picker opens.
pub(super) async fn export(
    ctx: &Context,
    names: Vec<SpaceSelector>,
    file: PathBuf,
    yes: bool,
) -> Result<i32> {
    if let [selector] = names.as_slice()
        && crate::client::workspaces::present_space(&ctx.paths, selector)
            .await?
            .is_none()
    {
        let output = Output::reserve(&file)?;
        let exported = export_removed(ctx, selector, &output).await?;
        return output.report(ctx, &exported);
    }
    if names.len() > 1 || file.is_dir() {
        ensure!(
            file != Path::new(STANDARD_STREAM),
            "several spaces export to a file each; name a directory with -o"
        );
        let spaces = selected_spaces(ctx, names).await?;
        return export_into(ctx, &spaces, file, yes).await;
    }
    let output = Output::reserve(&file)?;
    let named = names.into_iter().next();
    // The export ends its space, so it never takes the chat it runs in.
    let selector = ui::select_other_space(ctx, named, Fallback::CurrentDirectory).await?;
    let space = crate::client::workspaces::present_space(&ctx.paths, &selector)
        .await?
        .with_context(|| format!("no space {selector}"))?;
    let exported = export_space(ctx, space, yes, &output).await?;
    output.report(ctx, &exported)
}

/// `shoal move export --all`: export every space of `repository` to a file
/// of its own in `directory`.
pub(super) async fn export_all(
    ctx: &Context,
    repository: String,
    directory: PathBuf,
    yes: bool,
) -> Result<i32> {
    let spaces = repository_spaces(ctx, repository).await?;
    export_into(ctx, &spaces, directory, yes).await
}

/// The spaces `names` selects, or the current one or those the picker
/// offers without names.
pub(super) async fn selected_spaces(
    ctx: &Context,
    names: Vec<SpaceSelector>,
) -> Result<Vec<Space>> {
    let mut spaces = Vec::new();
    for selector in ui::select_spaces(ctx, names, true).await? {
        let space = crate::client::workspaces::present_space(&ctx.paths, &selector)
            .await?
            .with_context(|| format!("no space {selector}"))?;
        spaces.push(space);
    }
    Ok(spaces)
}

/// Export `spaces` to a file each in `directory`, once each one can be
/// exported, so a space that cannot be leaves every other one in place.
async fn export_into(
    ctx: &Context,
    spaces: &[Space],
    directory: PathBuf,
    yes: bool,
) -> Result<i32> {
    let directory = ExportDirectory::create(directory)?;
    let heading = format!(
        "Export {} spaces to {}",
        spaces.len(),
        directory.shown().display()
    );
    let planned = check_exports(ctx, spaces, &directory, yes, &heading, EXPORT).await?;
    export_each(ctx, planned, &directory).await
}

/// What a batch export is called in its refusals: its noun and verb.
pub(super) type Operation = (&'static str, &'static str);

const EXPORT: Operation = ("export", "export");

/// Where an export goes: a new file whose directory takes a file before
/// the space ends, so an unwritable destination fails first, or a
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
    fn report(self, ctx: &Context, exported: &ExportedSpace) -> Result<i32> {
        let name = exported.manifest.name();
        let (exported_space, opens) = match exported.manifest.copy {
            Some(_) => (format!("a copy of {name}"), "as a new space"),
            None => (name.to_string(), "on another machine"),
        };
        let Self::File { path } = &self else {
            let mut file = std::fs::File::open(self.file())?;
            crate::stdout::write(|stdout| {
                std::io::copy(&mut file, stdout)?;
                stdout.flush()
            })?;
            eprintln!("Exported {exported_space}");
            return Ok(0);
        };
        ctx.emit(
            &format!(
                "Exported {exported_space} to {}; shoal move import opens it {opens}",
                path.display()
            ),
            Exported {
                file: path.clone(),
                manifest: exported.manifest.clone(),
                removal: exported.removal.clone(),
            },
        )?;
        Ok(0)
    }
}

/// Export the removed space `selector` names, which needs no removal.
async fn export_removed(
    ctx: &Context,
    selector: &SpaceSelector,
    output: &Output,
) -> Result<ExportedSpace> {
    let exported = crate::client::transfer::export(
        &ctx.paths,
        selector.clone(),
        UnsavedWork::default(),
        output.file(),
    )
    .await?;
    warn(&exported);
    Ok(exported)
}

/// Export `space` to `output` once it can end for the export and the user
/// lets it end.
async fn export_space(
    ctx: &Context,
    space: Space,
    yes: bool,
    output: &Output,
) -> Result<ExportedSpace> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let check = check_export(ctx, space, output, &spaces).await?;
    check.ensure_children_exported(&[])?;
    confirm_ending(ctx, yes, format!("Export {}", check.label()), &[&check])?;
    export_checked(ctx, &check, output).await
}

/// End the space `check` describes and export it to `output`.
async fn export_checked(ctx: &Context, check: &Checked, output: &Output) -> Result<ExportedSpace> {
    let exported = match &check.space {
        Ending::Workspace(check) => {
            let space = check.workspace.id.clone();
            let unsaved = UnsavedWork::confirming(check);
            let exported = crate::client::transfer::export(
                &ctx.paths,
                SpaceSelector::id(&space),
                unsaved,
                output.file(),
            );
            super::super::workspaces::leaving(ctx, &check.workspace, exported).await?
        }
        Ending::Chat(chat) => {
            crate::client::transfer::export(
                &ctx.paths,
                SpaceSelector::id(&chat.id),
                check.chat_ending(),
                output.file(),
            )
            .await?
        }
    };
    warn_removal(exported.removal.as_ref());
    warn(&exported);
    Ok(exported)
}

/// Show the failure of the hook that `removal`, which ended a workspace for
/// its export, ran.
pub(super) fn warn_removal(removal: Option<&RemovalResult>) {
    if let Some(error) = removal.and_then(|removal| removal.hook_error.as_ref()) {
        eprintln!("warning: {error}");
    }
}

/// Show what the export left out.
pub(super) fn warn(exported: &ExportedSpace) {
    for warning in &exported.warnings {
        eprintln!("warning: {warning}");
    }
}

/// The spaces of `repository`: its workspaces and running chats.
pub(super) async fn repository_spaces(ctx: &Context, repository: String) -> Result<Vec<Space>> {
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let repo =
        crate::repository::select(&repositories, ui::repository_selector(repository)?).await?;
    let listed = crate::client::spaces::list(&ctx.paths).await?;
    let mut spaces: Vec<Space> = (listed.0.into_iter())
        .filter(|space| space.repository_id.as_ref() == Some(&repo.id))
        .filter(|space| space.worktree().is_some() || space.running)
        .collect();
    ensure!(
        !spaces.is_empty(),
        "{} has no spaces to export",
        crate::repository::name(repo)
    );
    spaces.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(spaces)
}

/// A space that passed its export check, with the file it goes to.
pub(super) struct PlannedExport {
    check: Checked,
    output: Output,
}

impl PlannedExport {
    pub(super) fn name(&self) -> &shoal_protocol::space_names::SpaceName {
        self.check.name()
    }

    /// The space's ID, which selects the workspace once it is removed.
    pub(super) fn id(&self) -> &shoal_ids::space_ids::SpaceId {
        self.check.id()
    }

    /// The space with what ending it would do.
    pub(super) fn checked(&self) -> &Checked {
        &self.check
    }

    /// The file its export goes to.
    pub(super) fn file(&self) -> PathBuf {
        self.output.file()
    }
}

/// Check every space before exporting any, refusing with each one that
/// cannot be exported, then let the user end all of them at once where that
/// needs the user, asking under `heading`; `operation` names the batch in
/// refusals. The chats that go with another of them go in its export, and
/// each space goes after the spaces below it.
pub(super) async fn check_exports(
    ctx: &Context,
    spaces: &[Space],
    directory: &ExportDirectory,
    yes: bool,
    heading: &str,
    operation: Operation,
) -> Result<Vec<PlannedExport>> {
    let tree = crate::client::spaces::list(&ctx.paths).await?;
    let spaces = exported_alone(spaces, &tree);
    let mut planned = Vec::new();
    let mut refusals = Vec::new();
    let exported: Vec<&SpaceId> = spaces.iter().map(|space| &space.id).collect();
    for space in &spaces {
        let export = async {
            let export = plan_export(ctx, space, directory, operation, &tree).await?;
            export.check.ensure_children_exported(&exported)?;
            Ok::<_, anyhow::Error>(export)
        };
        match export.await {
            Ok(export) => planned.push(export),
            Err(error) => refusals.push(format!("{}: {error:#}", space.name)),
        }
    }
    ensure!(
        refusals.is_empty(),
        "cannot {} these spaces, so none was ended:\n  {}",
        operation.1,
        refusals.join("\n  ")
    );
    let checks: Vec<&Checked> = planned.iter().map(|export| &export.check).collect();
    confirm_ending(ctx, yes, heading.to_owned(), &checks)?;
    Ok(planned)
}

/// Reserve the space's file in `directory` and check it as a batch's,
/// among the spaces `tree` shows.
async fn plan_export(
    ctx: &Context,
    space: &Space,
    directory: &ExportDirectory,
    operation: Operation,
    tree: &crate::model::Spaces,
) -> Result<PlannedExport> {
    let output = Output::reserve(&directory.file(&space.id))?;
    let check = check_export(ctx, space.clone(), &output, tree).await?;
    if let Ending::Workspace(check) = &check.space {
        super::super::workspaces::ensure_batch_removable(ctx, check, operation).await?;
    }
    Ok(PlannedExport { check, output })
}

/// Export each planned space, going on after one fails.
async fn export_each(
    ctx: &Context,
    planned: Vec<PlannedExport>,
    directory: &ExportDirectory,
) -> Result<i32> {
    let mut report = DirectoryReport::new(directory, false);
    for PlannedExport { check, output } in planned {
        let exported = export_checked(ctx, &check, &output).await;
        report.add(ctx, (check.name(), check.id()), exported);
    }
    report.finish(ctx)
}

#[cfg(test)]
mod tests;
