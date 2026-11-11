//! Copies of a worktree: a new linked worktree at another worktree's HEAD
//! that holds its index and files, uncommitted, untracked and ignored ones
//! included. Worktrunk checks files out of the commit when it creates a
//! worktree, so plain Git creates these without checking anything out.
use std::path::Path;

use anyhow::{Context, Result, ensure};

/// Create a linked worktree of `repo` at `path` at `commit`, on the new
/// branch `branch` or without one, with no file checked out and an empty
/// index. No hooks run.
pub async fn add_worktree(
    repo: &Path,
    path: &Path,
    branch: Option<&str>,
    commit: &str,
) -> Result<()> {
    let mut command = super::isolated_command_without_submodules(repo);
    command.args(["worktree", "add", "--no-checkout"]);
    match branch {
        Some(branch) => command.args(["-b", branch]),
        None => command.arg("--detach"),
    };
    command.arg("--").arg(path).arg(commit);
    super::output(command)
        .await
        .with_context(|| format!("create a worktree at {commit} in {}", path.display()))
        .map(drop)
}

/// Give the worktree at `copy`, which [`add_worktree`] created at `head`,
/// the index, worktree config and files of the worktree at `source`, which
/// must still be at `head` once its index is read; its `.git` file stays.
pub async fn fill(source: &Path, copy: &Path, head: &str) -> Result<()> {
    for file in ["index", "config.worktree"] {
        copy_admin_file(source, copy, file).await?;
    }
    let current = super::resolve_commit(source, "HEAD", super::run_isolated).await?;
    ensure!(
        current == head,
        "the workspace's HEAD moved while it was copied; copy it again"
    );
    crate::fsutil::copy::clone_worktree(source, copy).await
}

/// The paths in the worktree whose Git metadata lives elsewhere, which a
/// copy would share, so Git in the copy would change them: checked-out
/// submodules, and worktrees or submodules inside ignored directories.
pub async fn shared_metadata(worktree: &Path) -> Result<Vec<String>> {
    let mut paths = initialized_submodules(worktree).await?;
    let ignored = super::run_isolated(
        worktree,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )
    .await?;
    let ignored: Vec<String> = ignored
        .split('\0')
        .filter(|path| path.ends_with('/'))
        .map(|path| path.trim_end_matches('/').to_owned())
        .collect();
    let root = worktree.to_owned();
    paths.extend(crate::fsutil::off_runtime::run(move || linked_below(&root, ignored)).await?);
    Ok(paths)
}

/// The directories at or below `dirs` of `root` whose `.git` is a file,
/// which names Git metadata elsewhere, without following symbolic links.
fn linked_below(root: &Path, dirs: Vec<String>) -> Result<Vec<String>> {
    let mut linked = Vec::new();
    let mut pending: Vec<_> = dirs.into_iter().map(|dir| root.join(dir)).collect();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if entry.file_name() == ".git" && kind.is_file() {
                let relative = dir.strip_prefix(root).unwrap_or(&dir);
                linked.push(relative.display().to_string());
            } else if kind.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(linked)
}

/// The submodules checked out in the worktree.
async fn initialized_submodules(worktree: &Path) -> Result<Vec<String>> {
    let entries = super::run_isolated(worktree, &["ls-files", "--stage", "-z"]).await?;
    let submodules: Vec<String> = entries
        .split('\0')
        .filter_map(|entry| entry.strip_prefix("160000 "))
        .filter_map(|entry| entry.split_once('\t'))
        .map(|(_, path)| path.to_owned())
        .collect();
    let worktree = worktree.to_owned();
    crate::fsutil::off_runtime::run(move || {
        Ok(submodules
            .into_iter()
            .filter(|path| worktree.join(path).join(".git").exists())
            .collect())
    })
    .await
}

/// Copy `file` of `source`'s own Git directory into `copy`'s, when it has one.
async fn copy_admin_file(source: &Path, copy: &Path, file: &str) -> Result<()> {
    let from = admin_path(source, file).await?;
    let to = admin_path(copy, file).await?;
    crate::fsutil::off_runtime::run(move || match std::fs::copy(&from, &to) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("copy {}", from.display())),
    })
    .await
}

/// Where the worktree keeps `file` of its own Git directory.
async fn admin_path(worktree: &Path, file: &str) -> Result<std::path::PathBuf> {
    let path = super::run_isolated(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-path", file],
    )
    .await?;
    Ok(path.trim_end_matches('\n').into())
}

#[cfg(test)]
mod tests;
