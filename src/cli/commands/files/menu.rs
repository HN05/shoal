//! The picker `shoal files` opens on a terminal: a workspace's files, and a
//! row that adds one, where a key binding chooses what to do.
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

use super::{add, cells, remove};
use crate::{
    cli::{
        args::WorkspaceScope,
        context::Context,
        output::Palette,
        ui::{self, KeyBindings, WidthLimit},
    },
    daemon::workspace_files::AttachedFile,
};

const ADD_ENTRY: &str = "add-file";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FileAction {
    Print,
    PrintPath,
    Open,
    Add,
    Delete,
}

const BINDINGS: &[(&str, &str, FileAction)] = &[
    ("enter", "print", FileAction::Print),
    ("ctrl-y", "print path", FileAction::PrintPath),
    ("ctrl-o", "open", FileAction::Open),
    ("ctrl-a", "add", FileAction::Add),
    ("ctrl-d", "delete", FileAction::Delete),
];

/// Pick one of the workspace's `files`, or the add row, and act on it.
pub(super) async fn choose(
    ctx: &Context,
    workspace: String,
    files: &[AttachedFile],
) -> Result<i32> {
    let picked = ui::pick_with_keys(ctx, "File> ", entries(files), KeyBindings(BINDINGS))?;
    match resolve_action(picked.action, &picked.id)? {
        FileAction::Add => {
            let path = ui::input(ctx, "File to add")?;
            add(ctx, Some(workspace), vec![PathBuf::from(path)]).await
        }
        FileAction::Print => print(picked_file(files, &picked.id)?),
        FileAction::PrintPath => {
            outln!("{}", picked_file(files, &picked.id)?.path.display());
            Ok(0)
        }
        FileAction::Open => open(ctx, picked_file(files, &picked.id)?),
        FileAction::Delete => {
            let file = picked_file(files, &picked.id)?;
            let scope = WorkspaceScope {
                workspace: Some(workspace),
                all: false,
            };
            remove(ctx, scope, vec![], vec![file.id], false).await
        }
    }
}

/// One row per file, by its record, cut to fit the picker, then the add row.
fn entries(files: &[AttachedFile]) -> ui::Entries {
    let now = crate::time::unix_seconds() as i64;
    let rows = files.iter().map(|file| cells(file, now)).collect();
    let limit = ui::picker_width().map(|width| WidthLimit { column: 5, width });
    let labels = ui::aligned_to(rows, Palette::picker(), limit);
    files
        .iter()
        .map(|file| file.id.to_string())
        .zip(labels)
        .chain([(ADD_ENTRY.into(), "+ Add file".into())])
        .collect()
}

/// Enter on the add row adds; other actions need a file.
fn resolve_action(action: FileAction, id: &str) -> Result<FileAction> {
    if id != ADD_ENTRY {
        return Ok(action);
    }
    ensure!(
        matches!(action, FileAction::Print | FileAction::Add),
        "select a file for this action"
    );
    Ok(FileAction::Add)
}

/// Print the file's text; other files would garble the terminal.
fn print(file: &AttachedFile) -> Result<i32> {
    let bytes =
        std::fs::read(&file.path).with_context(|| format!("read {}", file.path.display()))?;
    let text = String::from_utf8(bytes).ok().with_context(|| {
        format!(
            "{} is not text; open it with ctrl-o or print its path with ctrl-y",
            file.name
        )
    })?;
    outln!("{}", text.trim_end_matches('\n'));
    Ok(0)
}

/// Open the file in the application the desktop chooses for it, and print
/// its path, also for a terminal elsewhere; workspace processes cannot.
fn open(ctx: &Context, file: &AttachedFile) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot open files on the user's screen; ask the user"
    );
    outln!("{}", file.path.display());
    crate::cli::desktop::open(&file.path)?;
    Ok(0)
}

fn picked_file<'a>(files: &'a [AttachedFile], id: &str) -> Result<&'a AttachedFile> {
    files
        .iter()
        .find(|file| file.id.to_string() == id)
        .context("picker returned an unknown file")
}

#[cfg(test)]
mod tests;
