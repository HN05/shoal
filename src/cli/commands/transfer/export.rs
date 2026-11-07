//! `shoal move export`: end spaces here, removing workspaces and stopping
//! chats' agents, and write each to a file, or to standard output.
use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail, ensure};

use crate::client::output::transfer::Exported;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    model::{Chat, PresentSpace},
    removal::{RemovalCheck, UnsavedWork},
    transfer::ExportedSpace,
};

use super::STANDARD_STREAM;

pub(super) mod copy;
mod directory;

use directory::DirectoryReport;
pub(super) use directory::ExportDirectory;

/// `shoal move export`: end the space here, or take an already removed
/// workspace, and write it to `file`. Several spaces, or any written to a
/// directory, go to a file each in it. The destination is checked before a
/// picker opens.
pub(super) async fn export(
    ctx: &Context,
    names: Vec<String>,
    file: PathBuf,
    yes: bool,
) -> Result<i32> {
    ensure_unscoped_export(ctx)?;
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
    ensure_unscoped_export(ctx)?;
    let spaces = repository_spaces(ctx, repository).await?;
    export_into(ctx, &spaces, directory, yes).await
}

/// The spaces `names` selects, or the current one or those the picker
/// offers without names.
pub(super) async fn selected_spaces(
    ctx: &Context,
    names: Vec<String>,
) -> Result<Vec<PresentSpace>> {
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
    spaces: &[PresentSpace],
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

pub(super) fn ensure_unscoped_export(ctx: &Context) -> Result<()> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot export spaces; ask the user to run shoal move export"
    );
    Ok(())
}

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
            None => (name.to_owned(), "on another machine"),
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

/// Export the removed workspace `selector` names, which needs no removal.
async fn export_removed(ctx: &Context, selector: &str, output: &Output) -> Result<ExportedSpace> {
    let exported = crate::client::transfer::export(
        &ctx.paths,
        SpaceSelector::parse(selector),
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
    space: PresentSpace,
    yes: bool,
    output: &Output,
) -> Result<ExportedSpace> {
    let check = check_export(ctx, space, output).await?;
    check.ensure_children_exported(&[])?;
    confirm_ending(ctx, yes, format!("Export {}", check.label()), &[&check])?;
    export_checked(ctx, &check, output).await
}

/// End the space `check` describes and export it to `output`.
async fn export_checked(ctx: &Context, check: &Checked, output: &Output) -> Result<ExportedSpace> {
    let exported = match check {
        Checked::Workspace(check) => {
            let space = check.workspace.id.clone();
            let unsaved = UnsavedWork::confirming(check);
            let exported = crate::client::transfer::export(
                &ctx.paths,
                SpaceSelector::parse(&space),
                unsaved,
                output.file(),
            );
            super::super::workspaces::leaving(ctx, &check.workspace, exported).await?
        }
        Checked::Chat(chat) => {
            crate::client::transfer::export(
                &ctx.paths,
                SpaceSelector::id(&chat.id),
                UnsavedWork::default(),
                output.file(),
            )
            .await?
        }
    };
    if let Some(error) = exported
        .removal
        .as_ref()
        .and_then(|removal| removal.hook_error.as_ref())
    {
        eprintln!("warning: {error}");
    }
    warn(&exported);
    Ok(exported)
}

/// Show what the export left out.
fn warn(exported: &ExportedSpace) {
    for warning in &exported.warnings {
        eprintln!("warning: {warning}");
    }
}

/// A space that passed its export check: a workspace with what its removal
/// would delete, or a running chat whose agent reported a session.
#[allow(
    clippy::large_enum_variant,
    reason = "one per exported space, moved into its planned export"
)]
pub(super) enum Checked {
    Workspace(RemovalCheck),
    Chat(Chat),
}

impl Checked {
    fn name(&self) -> &str {
        match self {
            Self::Workspace(check) => &check.workspace.name,
            Self::Chat(chat) => &chat.name,
        }
    }

    /// The space with its kind, as a confirmation names it.
    fn label(&self) -> String {
        match self {
            Self::Workspace(check) => format!("workspace {}", check.workspace.name),
            Self::Chat(chat) => format!("chat {}", chat.name),
        }
    }

    /// Refuse to export a workspace without the workspaces below it, which
    /// removing it here would take unexported, unless they are among the
    /// `exported` too.
    fn ensure_children_exported(&self, exported: &[&str]) -> Result<()> {
        match self {
            Self::Workspace(check) => ensure_children_exported(check, exported),
            Self::Chat(_) => Ok(()),
        }
    }
}

/// What ending `space` for an export would do, refusing an export that
/// would lose work or cut its own output off.
async fn check_export(ctx: &Context, space: PresentSpace, output: &Output) -> Result<Checked> {
    match space {
        PresentSpace::Workspace(workspace) => check_workspace_export(ctx, &workspace.id, output)
            .await
            .map(Checked::Workspace),
        PresentSpace::Chat(chat) => check_chat_export(chat).map(Checked::Chat),
    }
}

/// Refuse to export a chat whose agent reported no session, which would not
/// continue elsewhere, or the one this process runs in, which ending the
/// chat would end before the export finished.
fn check_chat_export(chat: Chat) -> Result<Chat> {
    if let Some(workspace) = &chat.workspace {
        bail!(
            "chat {} runs in workspace {}, and shoal move does not move a chat in a workspace",
            chat.name,
            workspace.name
        );
    }
    ensure!(
        chat.session.is_some(),
        "the agent of chat {} reported no session to continue elsewhere, so the chat keeps running",
        chat.name
    );
    ensure!(
        crate::env::chat().as_deref() != Some(chat.id.as_str()),
        "this process runs in chat {}, which the export ends; export it from another terminal",
        chat.name
    );
    Ok(chat)
}

/// What removing the workspace for an export would delete, refusing unsaved
/// files a checkpoint cannot keep and an output the removal would cut off.
async fn check_workspace_export(
    ctx: &Context,
    workspace: &str,
    output: &Output,
) -> Result<RemovalCheck> {
    let check =
        crate::client::removal::check(&ctx.paths, SpaceSelector::parse(workspace), true).await?;
    // Its removal keeps no record, which the package is built from.
    ensure!(
        !check.workspace.disposable,
        "workspace {} is disposable, so its removal keeps nothing to export",
        check.workspace.name
    );
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
    let (chats, workspaces): (Vec<_>, Vec<_>) =
        (check.children.iter()).partition(|child| child.kind == crate::model::SpaceKind::Chat);
    let chats: Vec<&str> = chats.iter().map(|chat| chat.name.as_str()).collect();
    ensure!(
        chats.is_empty(),
        "chats run in it or below it ({}), which shoal move does not move; remove them first",
        chats.join(", ")
    );
    let missing: Vec<&str> = workspaces
        .iter()
        .map(|child| child.name.as_str())
        .filter(|child| !exported.contains(child))
        .collect();
    ensure!(
        missing.is_empty(),
        "workspaces were split from it ({}); export them with it, as shoal move export --all does, or remove them first",
        missing.join(", ")
    );
    Ok(())
}

/// Ask under `heading`, unless `yes` confirms it, before ending the spaces
/// `checks` describe where that needs the user: a chat, which the user
/// talks with, or a workspace with holds that external sessions keep, which
/// the export releases, naming each hold's workspace when there are several.
fn confirm_ending(ctx: &Context, yes: bool, heading: String, checks: &[&Checked]) -> Result<()> {
    let chats: Vec<&Chat> = checks
        .iter()
        .filter_map(|check| match check {
            Checked::Chat(chat) => Some(chat),
            Checked::Workspace(_) => None,
        })
        .collect();
    let held: Vec<&RemovalCheck> = checks
        .iter()
        .filter_map(|check| match check {
            Checked::Workspace(check) if !check.workspace.holds.is_empty() => Some(check),
            _ => None,
        })
        .collect();
    if yes || (chats.is_empty() && held.is_empty()) {
        return Ok(());
    }
    let mut action = heading;
    if !chats.is_empty() {
        action.push_str("\nEnds these chats here, stopping their agents:");
        for chat in &chats {
            action.push_str(&format!("\n  {} ({})", chat.name, chat.agent));
        }
    }
    if !held.is_empty() {
        action.push_str("\nRemoves the workspaces here and releases these holds:");
    }
    for check in &held {
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
    ensure!(ui::confirm(ctx, &action, "--yes")?, "export canceled");
    Ok(())
}

/// The spaces of `repository`, by name: its workspaces and running chats.
pub(super) async fn repository_spaces(
    ctx: &Context,
    repository: String,
) -> Result<Vec<PresentSpace>> {
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let repo =
        crate::repository::select(&repositories, ui::repository_selector(repository)?).await?;
    let workspaces = crate::client::workspaces::list(&ctx.paths)
        .await?
        .into_iter()
        .filter(|workspace| workspace.repository_id == repo.id)
        .map(PresentSpace::Workspace);
    let chats = crate::client::chats::list(&ctx.paths)
        .await?
        .into_iter()
        .filter(|chat| chat.repository_id.as_ref() == Some(&repo.id))
        .map(PresentSpace::Chat);
    let mut spaces: Vec<PresentSpace> = workspaces.chain(chats).collect();
    ensure!(
        !spaces.is_empty(),
        "{} has no spaces to export",
        crate::repository::name(repo)
    );
    spaces.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(spaces)
}

/// A space that passed its export check, with the file it goes to.
pub(super) struct PlannedExport {
    check: Checked,
    output: Output,
}

impl PlannedExport {
    pub(super) fn name(&self) -> &str {
        self.check.name()
    }

    /// The chat the export ends, which keeps no record here once it ends,
    /// unlike a removed workspace, from which it opens again without its
    /// package.
    pub(super) fn chat(&self) -> Option<&Chat> {
        match &self.check {
            Checked::Chat(chat) => Some(chat),
            Checked::Workspace(_) => None,
        }
    }

    /// End the space and export it, returning the file it went to.
    pub(super) async fn export(self, ctx: &Context) -> Result<PathBuf> {
        export_checked(ctx, &self.check, &self.output).await?;
        Ok(self.output.file())
    }
}

/// Check every space before exporting any, refusing with each one that
/// cannot be exported, then let the user end all of them at once where that
/// needs the user, asking under `heading`; `operation` names the batch in
/// refusals.
pub(super) async fn check_exports(
    ctx: &Context,
    spaces: &[PresentSpace],
    directory: &ExportDirectory,
    yes: bool,
    heading: &str,
    operation: Operation,
) -> Result<Vec<PlannedExport>> {
    let mut planned = Vec::new();
    let mut refusals = Vec::new();
    let exported: Vec<&str> = spaces.iter().map(PresentSpace::name).collect();
    for space in spaces {
        let export = async {
            let export = plan_export(ctx, space, directory, operation).await?;
            export.check.ensure_children_exported(&exported)?;
            Ok::<_, anyhow::Error>(export)
        };
        match export.await {
            Ok(export) => planned.push(export),
            Err(error) => refusals.push(format!("{}: {error:#}", space.name())),
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

/// Reserve the space's file in `directory` and check it as a batch's.
async fn plan_export(
    ctx: &Context,
    space: &PresentSpace,
    directory: &ExportDirectory,
    operation: Operation,
) -> Result<PlannedExport> {
    let output = Output::reserve(&directory.file(space.name()))?;
    let check = check_export(ctx, space.clone(), &output).await?;
    if let Checked::Workspace(check) = &check {
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
        report.add(ctx, check.name(), exported);
    }
    report.finish(ctx)
}

#[cfg(test)]
mod tests;
