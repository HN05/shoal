//! The picker `shoal files` opens on a terminal.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use crate::client::files::FileRead;
use crate::fetched_copies;
use anyhow::{Context as _, Result, ensure};

use super::{add, cells, remove};
use crate::cli::{
    args::SpaceFilter,
    context::Context,
    output::Palette,
    ui::{self, Key, KeyBindings, WidthLimit},
};
use shoal_protocol::files::AttachedFile;
use shoal_protocol::space_names::SpaceSelector;

const ADD_ENTRY: &str = "add-file";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FileAction {
    Print,
    PrintPath,
    Open,
    Add,
    Delete,
}

const BINDINGS: &[(Key, &str, FileAction)] = &[
    (Key::Enter, "print", FileAction::Print),
    (Key::Ctrl('y'), "print path", FileAction::PrintPath),
    (Key::Ctrl('o'), "open", FileAction::Open),
    (Key::Ctrl('a'), "add", FileAction::Add),
    (Key::Ctrl('d'), "delete", FileAction::Delete),
];

pub(super) async fn choose(
    ctx: &Context,
    space: SpaceSelector,
    files: &[AttachedFile],
) -> Result<i32> {
    let picked = ui::pick_with_keys(ctx, "File> ", entries(files), KeyBindings(BINDINGS))?;
    match resolve_action(picked.action, &picked.id)? {
        FileAction::Add => {
            let path = ui::input(ctx, "File to add")?;
            add(ctx, Some(space), vec![path]).await
        }
        FileAction::Print => print(ctx, picked_file(files, &picked.id)?).await,
        FileAction::PrintPath => {
            outln!("{}", kept_here(picked_file(files, &picked.id)?)?.display());
            Ok(0)
        }
        FileAction::Open => open(ctx, picked_file(files, &picked.id)?).await,
        FileAction::Delete => {
            let file = picked_file(files, &picked.id)?;
            let filter = SpaceFilter {
                space: Some(space),
                all: false,
            };
            remove(ctx, filter, vec![], vec![file.id], false).await
        }
    }
}

fn entries(files: &[AttachedFile]) -> ui::Entries {
    let now = crate::time::UnixTime::now();
    let rows = files.iter().map(|file| cells(file, now)).collect();
    let limit = ui::picker_width().map(|width| WidthLimit { column: 6, width });
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

/// Only text prints: other files would garble the terminal.
async fn print(ctx: &Context, file: &AttachedFile) -> Result<i32> {
    let bytes = read(ctx, file).await?.to_end().await?;
    let text = String::from_utf8(bytes).ok().with_context(|| {
        format!(
            "{} is not text; open it with ctrl-o or print its path with ctrl-y",
            file.name
        )
    })?;
    outln!("{}", text.trim_end_matches('\n'));
    Ok(0)
}

/// Open the file in the application the desktop chooses for it, and print its
/// path. Workspace processes cannot.
async fn open(ctx: &Context, file: &AttachedFile) -> Result<i32> {
    ensure!(
        !crate::process::scoped::inherits_scope(&ctx.paths.state),
        "workspace processes cannot open files on the user's screen; ask the user"
    );
    let path = match file.path() {
        Some(path) => path.to_path_buf(),
        None => fetch(ctx, file).await?,
    };
    outln!("{}", path.display());
    crate::cli::desktop::open(&path)?;
    Ok(0)
}

async fn read(ctx: &Context, file: &AttachedFile) -> Result<FileRead> {
    let space = SpaceSelector::id(&file.space.id);
    crate::client::files::read(&ctx.paths, space, file.id).await
}

/// Fetch the file from the machine that keeps it into the directory where
/// this machine keeps such copies, replacing an earlier one; its path.
async fn fetch(ctx: &Context, file: &AttachedFile) -> Result<PathBuf> {
    let space = SpaceSelector::id(&file.space.id);
    let directory = crate::client::files::fetched_directory(&ctx.paths, space).await?;
    let name = Path::new(&file.name)
        .file_name()
        .context("the file has no name")?;
    let path = directory.join(file.id.to_string()).join(name);
    write_whole(&path, read(ctx, file).await?).await?;
    Ok(path)
}

/// Write what `read` brings to `path`, which takes it only once it has
/// come whole, stamped as fetched.
async fn write_whole(path: &Path, mut read: FileRead) -> Result<()> {
    let parent = path.parent().context("a fetched file has a directory")?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let partial = parent.join(fetched_copies::FETCHING);
    let mut out =
        std::fs::File::create(&partial).with_context(|| format!("create {}", partial.display()))?;
    while let Some(bytes) = read.next().await? {
        out.write_all(&bytes)
            .with_context(|| format!("write {}", partial.display()))?;
    }
    std::fs::rename(&partial, path).with_context(|| format!("write {}", path.display()))?;
    fetched_copies::stamp(path).with_context(|| format!("stamp {}", path.display()))
}

fn kept_here(file: &AttachedFile) -> Result<&std::path::Path> {
    file.path()
        .with_context(|| format!("{} is kept {}", file.name, file.copy.label()))
}

fn picked_file<'a>(files: &'a [AttachedFile], id: &str) -> Result<&'a AttachedFile> {
    files
        .iter()
        .find(|file| file.id.to_string() == id)
        .context("picker returned an unknown file")
}

#[cfg(test)]
mod tests;
