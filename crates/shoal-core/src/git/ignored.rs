//! The ignored files of a repository's main checkout, such as `.env`,
//! copied into a new worktree before its setup. Ported from Worktrunk
//! 0.81.0's `wt step copy-ignored` (<https://github.com/max-sixty/worktrunk>),
//! used under its MIT License:
//!
//! Copyright (c) 2025 Maximilian Roos
//!
//! Permission is hereby granted, free of charge, to any person obtaining a
//! copy of this software and associated documentation files (the
//! "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to permit
//! persons to whom the Software is furnished to do so, subject to the
//! following conditions:
//!
//! The above copyright notice and this permission notice shall be included
//! in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//! OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN
//! NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
//! DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
//! OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
//! USE OR OTHER DEALINGS IN THE SOFTWARE.
use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::fsutil::off_runtime;

/// The file in the main checkout whose gitignore-style patterns select the
/// ignored entries to copy, a convention other worktree tools share.
const INCLUDE_FILE: &str = ".worktreeinclude";

/// Directories that hold another version control system's or tool's state
/// for the checkout they are in, never copied into another.
const ALWAYS_EXCLUDED: &[&str] = &[
    ".bzr",
    ".conductor",
    ".entire",
    ".hg",
    ".jj",
    ".pijul",
    ".sl",
    ".svn",
    ".worktrees",
];

/// Variables that point Git at a repository, work tree or index other than
/// the directory a command names, which setup's environment can set.
const DISCOVERY_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
];

/// An ignored entry of the main checkout, as `git ls-files --directory`
/// lists it: a whole directory when nothing in it is tracked.
#[derive(Debug)]
struct Entry {
    /// The path relative to the checkout.
    path: PathBuf,
    is_dir: bool,
}

/// Copy the ignored entries of the main checkout of the repository of the
/// worktree at `workspace` into it, keeping every file it already has, and
/// return how many files it copied. A `.worktreeinclude` in the checkout
/// limits the copy to the entries it matches; `exclude` holds gitignore-style
/// patterns of entries to leave out. Entries that hold another worktree, or
/// another tool's state, are never copied.
pub async fn copy(workspace: &Path, exclude: &[String]) -> Result<u64> {
    let worktrees = registered_worktrees(workspace).await?;
    let checkout = worktrees
        .first()
        .context("the repository lists no main checkout")?
        .clone();
    let workspace = off_runtime::canonicalize(workspace).await?;
    if checkout == workspace {
        return Ok(0);
    }
    let entries = ignored_entries(&checkout).await?;
    let parts = Matchers::new(&checkout, exclude)
        .await?
        .selected_parts(&checkout, entries)
        .await?;
    let mut copied = 0;
    for part in parts {
        if !holds_worktree(&checkout, &part, &worktrees) {
            copied += copy_entry(&checkout, &workspace, &part.path).await?;
        }
    }
    Ok(copied)
}

/// The canonical paths of the repository's worktrees, the main checkout
/// first, as far as they still exist.
async fn registered_worktrees(workspace: &Path) -> Result<Vec<PathBuf>> {
    let worktrees = super::worktrees(workspace, run).await?;
    let mut paths = Vec::with_capacity(worktrees.len());
    for worktree in worktrees {
        let path = off_runtime::canonicalize(&worktree.path)
            .await
            .unwrap_or(worktree.path);
        paths.push(path);
    }
    Ok(paths)
}

/// The ignored entries of `checkout`, whole directories where nothing in
/// them is tracked. Paths are bytes, which need not be UTF-8.
async fn ignored_entries(checkout: &Path) -> Result<Vec<Entry>> {
    let mut command = command(checkout);
    command.args([
        "ls-files",
        "-z",
        "--ignored",
        "--exclude-standard",
        "--others",
        "--directory",
    ]);
    let listing = super::bounded(command)
        .checked()
        .await
        .with_context(|| format!("list the ignored files of {}", checkout.display()))?
        .stdout;
    Ok(listing
        .split(|&byte| byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (path, is_dir) = match entry.strip_suffix(b"/") {
                Some(path) => (path, true),
                None => (entry, false),
            };
            Entry {
                path: OsString::from_vec(path.to_vec()).into(),
                is_dir,
            }
        })
        .collect())
}

/// Isolated Git in `dir`, whatever Git discovery overrides the setup
/// environment carries.
fn command(dir: &Path) -> tokio::process::Command {
    let mut command = super::isolated_command(dir);
    for name in DISCOVERY_OVERRIDES {
        command.env_remove(name);
    }
    command
}

/// The output of the Git `command` with `args` in `dir`.
async fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let mut command = command(dir);
    command.args(args);
    super::output(command).await
}

/// Which entries the checkout's `.worktreeinclude` and the configured
/// excludes select.
struct Matchers {
    include: Option<Gitignore>,
    exclude: Gitignore,
}

impl Matchers {
    async fn new(checkout: &Path, exclude: &[String]) -> Result<Self> {
        let (checkout, exclude) = (checkout.to_owned(), exclude.to_vec());
        off_runtime::run(move || {
            Ok(Self {
                include: include_matcher(&checkout)?,
                exclude: exclude_matcher(&checkout, &exclude)?,
            })
        })
        .await
    }

    /// The parts of `entries` to copy, read on the blocking pool.
    async fn selected_parts(self, checkout: &Path, entries: Vec<Entry>) -> Result<Vec<Entry>> {
        let checkout = checkout.to_owned();
        off_runtime::run(move || {
            let mut parts = Vec::new();
            for entry in entries {
                parts.extend(self.parts(&checkout, entry)?);
            }
            Ok(parts)
        })
        .await
    }

    /// The parts of `entry` to copy: the entry itself when everything in it
    /// is selected, otherwise the selected parts below it. Git lists an
    /// ignored directory as one entry, so a pattern for something inside it
    /// takes a walk of the directory, which only patterns call for.
    fn parts(&self, checkout: &Path, entry: Entry) -> Result<Vec<Entry>> {
        if self.leaves_out(&entry) {
            return Ok(Vec::new());
        }
        if !entry.is_dir || (self.include.is_none() && self.exclude.is_empty()) {
            return Ok(vec![entry]);
        }
        let dir = checkout.join(&entry.path);
        let mut parts = Vec::new();
        let mut whole = self.includes(&entry);
        for child in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let child = child?;
            let child = Entry {
                path: entry.path.join(child.file_name()),
                is_dir: child.file_type()?.is_dir(),
            };
            let child_path = child.path.clone();
            let child_parts = self.parts(checkout, child)?;
            whole &= matches!(&child_parts[..], [part] if part.path == child_path);
            parts.extend(child_parts);
        }
        Ok(if whole { vec![entry] } else { parts })
    }

    /// `.worktreeinclude`, if any, matches the entry or a directory it is in,
    /// which lets a directory pattern cover a file Git lists on its own,
    /// beside tracked files in its directory.
    fn includes(&self, entry: &Entry) -> bool {
        self.include
            .as_ref()
            .is_none_or(|include| matches(include, entry))
    }

    /// The entry is not to be copied, whatever is in it: an exclude pattern
    /// matches it or a directory it is in, it is another tool's state, or
    /// `.worktreeinclude` leaves out a file.
    fn leaves_out(&self, entry: &Entry) -> bool {
        matches(&self.exclude, entry)
            || (entry.is_dir
                && entry
                    .path
                    .file_name()
                    .is_some_and(|name| ALWAYS_EXCLUDED.iter().any(|excluded| name == *excluded)))
            || (!entry.is_dir && !self.includes(entry))
    }
}

/// Whether `matcher` matches the entry or a directory it is in.
fn matches(matcher: &Gitignore, entry: &Entry) -> bool {
    matcher
        .matched_path_or_any_parents(&entry.path, entry.is_dir)
        .is_ignore()
}

/// The patterns of the checkout's `.worktreeinclude`, when it has one.
fn include_matcher(checkout: &Path) -> Result<Option<Gitignore>> {
    let path = checkout.join(INCLUDE_FILE);
    if !path.try_exists()? {
        return Ok(None);
    }
    let mut builder = GitignoreBuilder::new(checkout);
    if let Some(error) = builder.add(&path) {
        return Err(error).with_context(|| format!("invalid {}", path.display()));
    }
    Ok(Some(
        builder
            .build()
            .with_context(|| format!("invalid {}", path.display()))?,
    ))
}

/// The configured `setup.copy_ignored_exclude` patterns.
fn exclude_matcher(checkout: &Path, patterns: &[String]) -> Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(checkout);
    for pattern in patterns {
        builder
            .add_line(None, pattern)
            .with_context(|| format!("invalid setup.copy_ignored_exclude pattern {pattern:?}"))?;
    }
    builder
        .build()
        .context("invalid setup.copy_ignored_exclude patterns")
}

/// Whether the entry is, holds or lies inside a worktree other than the
/// checkout, which a copy would nest inside the new one, its `.git` file
/// included.
fn holds_worktree(checkout: &Path, entry: &Entry, worktrees: &[PathBuf]) -> bool {
    let path = checkout.join(&entry.path);
    worktrees.iter().any(|worktree| {
        worktree != checkout && (worktree.starts_with(&path) || path.starts_with(worktree))
    })
}

/// Copy the entry at `relative` of `checkout` into `workspace`, keeping
/// what the workspace has there, and return how many files it copied. Its
/// directory must resolve inside the workspace, so nothing goes through a
/// symbolic link to elsewhere.
async fn copy_entry(checkout: &Path, workspace: &Path, relative: &Path) -> Result<u64> {
    let destination = workspace.join(relative);
    if let Some(parent) = destination.parent() {
        let (parent, workspace) = (parent.to_owned(), workspace.to_owned());
        off_runtime::run(move || create_inside(&workspace, &parent)).await?;
    }
    crate::fsutil::copy::copy_missing(&checkout.join(relative), &destination)
        .await
        .with_context(|| format!("copy {}", relative.display()))
}

/// Create the directory `dir`, which must resolve inside `root`.
fn create_inside(root: &Path, dir: &Path) -> Result<()> {
    let mut existing = dir;
    while !existing.try_exists()? {
        existing = existing.parent().unwrap_or(root);
    }
    let resolved = std::fs::canonicalize(existing)?;
    anyhow::ensure!(
        resolved.starts_with(root),
        "{} resolves outside the workspace, to {}",
        existing.display(),
        resolved.display()
    );
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))
}

#[cfg(test)]
mod tests;
