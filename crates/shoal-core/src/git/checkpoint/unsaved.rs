//! The size of the files a checkpoint would add to HEAD, read from one
//! `git status` where it tells which files `git diff HEAD` lists.
use std::path::Path;

use anyhow::Result;

use crate::fsutil::off_runtime;

use super::run_isolated;

/// The size of the files a snapshot would add to HEAD: tracked changes and
/// untracked files, excluding ignored ones. `None` when an untracked directory
/// is a nested repository or a submodule changed, since a snapshot records
/// only the commit of either, not its files.
pub async fn unsaved_bytes(worktree: &Path) -> Result<Option<u64>> {
    let Some(unsaved) = read_status(worktree).await? else {
        return Ok(None);
    };
    let changed = match any_exists(worktree, &unsaved.undecided).await? {
        true => changed_by_diff(worktree).await?,
        false => unsaved.changed,
    };
    let files = changed.into_iter().chain(unsaved.untracked).collect();
    Ok(Some(total_size(worktree, files).await?))
}

/// The unsaved files one status read lists, each counted as the files
/// `git diff HEAD` and `git ls-files --others` list them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Unsaved {
    /// Changed paths `git diff HEAD` lists.
    changed: Vec<String>,
    /// Changed paths the status cannot tell whether `git diff HEAD` lists,
    /// which matters only where their file exists, since a missing file
    /// adds nothing.
    undecided: Vec<String>,
    untracked: Vec<String>,
}

/// The unsaved files of `worktree`; `None` when it holds an untracked
/// nested repository or a changed submodule.
async fn read_status(worktree: &Path) -> Result<Option<Unsaved>> {
    let status = run_isolated(
        worktree,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
            "--no-renames",
        ],
    )
    .await?;
    Ok(parse_status(&status))
}

/// The [`Unsaved`] files the `-z` porcelain v2 `status` lists; `None` when
/// it lists an untracked nested repository or a changed submodule.
fn parse_status(status: &str) -> Option<Unsaved> {
    let mut unsaved = Unsaved::default();
    let mut entries = status.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let (kind, rest) = entry.split_once(' ')?;
        match kind {
            // Git lists a nested repository as its directory, without
            // descending.
            "?" if rest.ends_with('/') => return None,
            "?" => unsaved.untracked.push(rest.to_owned()),
            "1" | "2" => {
                let changed = Changed::parse(rest, if kind == "1" { 8 } else { 9 })?;
                if changed.submodule.starts_with('S') {
                    return None;
                }
                // A rename's original path follows as its own entry.
                if kind == "2" {
                    unsaved.undecided.extend(entries.next().map(str::to_owned));
                }
                unsaved.add(changed.path, kind == "1" && diff_lists(changed.code));
            }
            "u" => unsaved.add(rest.splitn(10, ' ').last()?, false),
            _ => {}
        }
    }
    Some(unsaved)
}

impl Unsaved {
    /// Count the changed `path`, which `git diff HEAD` lists where `listed`
    /// and may list otherwise.
    fn add(&mut self, path: &str, listed: bool) {
        match listed {
            true => self.changed.push(path.to_owned()),
            false => self.undecided.push(path.to_owned()),
        }
    }
}

/// A changed or renamed entry of a porcelain v2 status.
struct Changed<'a> {
    /// The index's change from HEAD, then the worktree's from the index.
    code: &'a str,
    /// `N...` for a file, `S` and flags for a submodule.
    submodule: &'a str,
    path: &'a str,
}

impl<'a> Changed<'a> {
    /// The entry whose text after its kind is `rest`, of `fields` fields,
    /// the path last.
    fn parse(rest: &'a str, fields: usize) -> Option<Self> {
        let mut fields = rest.splitn(fields, ' ');
        Some(Self {
            code: fields.next()?,
            submodule: fields.next()?,
            path: fields.last()?,
        })
    }
}

/// Whether the status code `code` shows that `git diff HEAD` lists its
/// path; `false` where it cannot tell.
fn diff_lists(code: &str) -> bool {
    let mut code = code.chars();
    let (Some(index), Some(worktree)) = (code.next(), code.next()) else {
        return false;
    };
    match (index, worktree) {
        // An intent-to-add entry.
        ('.', 'A') => false,
        // HEAD has the index's copy, which the worktree's differs from.
        ('.', _) => true,
        // A diff may take a deletion and an addition as one rename.
        ('D', _) => false,
        // HEAD has no copy, or the worktree has none, which adds nothing.
        ('A', _) | (_, 'D') => true,
        // The worktree has the index's copy, which differs from HEAD's.
        (_, '.') => true,
        // The worktree differs from the index, perhaps back to HEAD's copy.
        _ => false,
    }
}

/// Whether a file exists at any of `paths` in `worktree`.
async fn any_exists(worktree: &Path, paths: &[String]) -> Result<bool> {
    if paths.is_empty() {
        return Ok(false);
    }
    let (worktree, paths) = (worktree.to_owned(), paths.to_vec());
    off_runtime::run(move || {
        Ok((paths.iter()).any(|path| std::fs::symlink_metadata(worktree.join(path)).is_ok()))
    })
    .await
}

/// The changed paths `git diff HEAD` lists.
async fn changed_by_diff(worktree: &Path) -> Result<Vec<String>> {
    let changed = run_isolated(worktree, &["diff", "-z", "--name-only", "HEAD", "--"]).await?;
    Ok((changed.split('\0'))
        .filter(|file| !file.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The total size of the `files` in `worktree`, once for each time they are
/// listed.
async fn total_size(worktree: &Path, files: Vec<String>) -> Result<u64> {
    let worktree = worktree.to_owned();
    off_runtime::run(move || {
        let mut bytes = 0;
        for file in files {
            // A deleted tracked file is listed as changed and adds nothing.
            if let Ok(metadata) = std::fs::symlink_metadata(worktree.join(file)) {
                bytes += metadata.len();
            }
        }
        Ok(bytes)
    })
    .await
}

#[cfg(test)]
mod tests;
