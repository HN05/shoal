//! Checkpoints: commits that keep a worktree's HEAD and unsaved files
//! reachable under private refs until their records expire.
use std::path::Path;

use anyhow::{Context, Result, ensure};

use super::{isolated_command_without_submodules, run_isolated, run_without_submodules};

const REFS: &str = "refs/shoal/checkpoints/";
/// Where removals kept their snapshots before checkpoints replaced them.
const LEGACY_REFS: &str = "refs/shoal/removed/";

/// The ref that keeps a checkpoint's commit reachable. It is named by the
/// commit, so whichever checkpoints keep that commit own it.
pub fn reference(commit: &str) -> String {
    format!("{REFS}{commit}")
}

/// Every checkpoint ref in the repository, with the commit it keeps.
pub async fn saved_refs(repo: &Path) -> Result<Vec<(String, String)>> {
    refs(repo, REFS).await
}

/// The refs removals kept before checkpoints, with the commit each keeps.
pub async fn legacy_refs(repo: &Path) -> Result<Vec<(String, String)>> {
    refs(repo, LEGACY_REFS).await
}

async fn refs(repo: &Path, prefix: &str) -> Result<Vec<(String, String)>> {
    let refs = run_isolated(
        repo,
        &["for-each-ref", "--format=%(refname) %(objectname)", prefix],
    )
    .await?;
    Ok(refs
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(reference, commit)| (reference.to_owned(), commit.to_owned()))
        .collect())
}

/// The size of the files a snapshot would add to HEAD: tracked changes and
/// untracked files, excluding ignored ones. `None` when an untracked directory
/// is a nested repository or a submodule changed, since a snapshot records
/// only the commit of either, not its files.
pub async fn unsaved_bytes(worktree: &Path) -> Result<Option<u64>> {
    let changed = run_isolated(worktree, &["diff", "-z", "--name-only", "HEAD", "--"]).await?;
    let untracked = run_isolated(
        worktree,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )
    .await?;
    // Git lists a nested repository as its directory, without descending.
    if untracked.split('\0').any(|file| file.ends_with('/')) || changed_submodule(worktree).await? {
        return Ok(None);
    }
    let mut bytes = 0;
    for file in [changed, untracked]
        .iter()
        .flat_map(|files| files.split('\0'))
        .filter(|file| !file.is_empty())
    {
        // A deleted tracked file is listed as changed and adds nothing.
        if let Ok(metadata) = std::fs::symlink_metadata(worktree.join(file)) {
            bytes += metadata.len();
        }
    }
    Ok(Some(bytes))
}

/// Whether a submodule has a new commit, tracked changes or untracked files.
async fn changed_submodule(worktree: &Path) -> Result<bool> {
    let status = run_isolated(
        worktree,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=no",
            "--ignore-submodules=none",
        ],
    )
    .await?;
    let mut entries = status.split('\0');
    while let Some(entry) = entries.next() {
        let mut fields = entry.split(' ');
        let kind = fields.next();
        // Changed entries name their submodule state third: `S` and flags.
        if matches!(kind, Some("1" | "2")) && fields.nth(1).is_some_and(|sub| sub.starts_with('S'))
        {
            return Ok(true);
        }
        // A rename's original path follows as its own entry.
        if kind == Some("2") {
            entries.next();
        }
    }
    Ok(false)
}

/// The branch and HEAD a snapshot was taken at, and its commit.
pub struct Snapshot {
    /// `None` for a detached HEAD.
    pub branch: Option<String>,
    pub head: String,
    /// HEAD, or a commit on top of it with the unsaved files.
    pub commit: String,
}

/// A commit on top of HEAD with the worktree's files, ignored ones excepted
/// unless staged, or HEAD itself when `files` is false or nothing is unsaved.
/// A temporary index keeps the worktree's own index and files untouched. A
/// branch switch or commit while it is taken fails it, so its HEAD always
/// matches its files.
pub async fn snapshot(worktree: &Path, message: &str, files: bool) -> Result<Snapshot> {
    let (branch, head) = head_state(worktree).await?;
    let commit = snapshot_commit(worktree, &head, message, files).await?;
    ensure!(
        head_state(worktree).await? == (branch.clone(), head.clone()),
        "HEAD moved while the checkpoint was taken; take it again"
    );
    Ok(Snapshot {
        branch,
        head,
        commit,
    })
}

/// The branch checked out, `None` when detached, and the commit HEAD names.
async fn head_state(worktree: &Path) -> Result<(Option<String>, String)> {
    let branch = run_isolated(worktree, &["branch", "--show-current"]).await?;
    let branch = branch.trim();
    let head = run_isolated(worktree, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    Ok((
        (!branch.is_empty()).then(|| branch.to_owned()),
        head.trim().to_owned(),
    ))
}

async fn snapshot_commit(
    worktree: &Path,
    head: &str,
    message: &str,
    files: bool,
) -> Result<String> {
    let status = run_isolated(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .await?;
    if !files || status.is_empty() {
        return Ok(head.to_owned());
    }
    let index = tempfile::tempdir().context("create a temporary index directory")?;
    let index = index.path().join("index");
    let in_index = async |args: &[&str]| {
        let mut command = isolated_command_without_submodules(worktree);
        command
            .env("GIT_INDEX_FILE", &index)
            .env("GIT_AUTHOR_NAME", "Shoal")
            .env("GIT_AUTHOR_EMAIL", "shoal@localhost")
            .env("GIT_COMMITTER_NAME", "Shoal")
            .env("GIT_COMMITTER_EMAIL", "shoal@localhost")
            .args(args);
        super::output(command).await
    };
    // The worktree's own index keeps paths staged despite ignore rules.
    let own = run_isolated(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )
    .await?;
    std::fs::copy(own.trim(), &index).context("copy the worktree's index")?;
    in_index(&["add", "--all", "--", ":/"]).await?;
    let tree = in_index(&["write-tree"]).await?;
    let commit = in_index(&["commit-tree", tree.trim(), "-p", head, "-m", message]).await?;
    Ok(commit.trim().to_owned())
}

/// Whether two snapshots keep the same files.
pub async fn same_files(repo: &Path, first: &str, second: &str) -> Result<bool> {
    let trees = run_isolated(
        repo,
        &[
            "rev-parse",
            &format!("{first}^{{tree}}"),
            &format!("{second}^{{tree}}"),
        ],
    )
    .await?;
    let mut trees = trees.lines();
    Ok(trees.next() == trees.next())
}

/// Write the files of `snapshot` into the worktree, leaving the index alone:
/// changed and untracked files come back as unsaved changes, and files the
/// snapshot lacks are deleted.
pub async fn apply(worktree: &Path, snapshot: &str) -> Result<()> {
    let mut command = isolated_command_without_submodules(worktree);
    command.args([
        "restore",
        &format!("--source={snapshot}"),
        "--worktree",
        "--",
        ":/",
    ]);
    super::output(command).await?;
    Ok(())
}

/// Move the worktree's branch, or its detached HEAD, to `head` and replace
/// its unsaved files with those of `snapshot`, deleting tracked changes and
/// untracked files the snapshot lacks. Untracked files are cleaned under the
/// current ignore rules, before the reset can change them, so ignored files
/// stay; [`ignored_collisions`] finds those the rewind would replace.
pub async fn rewind(worktree: &Path, head: &str, snapshot: &str) -> Result<()> {
    run_without_submodules(worktree, &["clean", "--quiet", "--force", "-d", "--", ":/"]).await?;
    run_without_submodules(worktree, &["reset", "--quiet", "--hard", head, "--"]).await?;
    if snapshot != head {
        apply(worktree, snapshot).await?;
    }
    Ok(())
}

/// The paths of `head` and `snapshot` that collide with an ignored file or
/// directory in the worktree: the same path, or one inside the other.
/// Rewinding to them would delete or overwrite those files, and no
/// checkpoint keeps ignored files.
pub async fn ignored_collisions(
    worktree: &Path,
    head: &str,
    snapshot: &str,
) -> Result<Vec<String>> {
    let ignored = run_isolated(
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
    let ignored: Vec<&str> = ignored
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(|path| path.trim_end_matches('/'))
        .collect();
    if ignored.is_empty() {
        return Ok(Vec::new());
    }
    let mut paths = std::collections::BTreeSet::new();
    for commit in [head, snapshot] {
        let tree = run_isolated(
            worktree,
            &["ls-tree", "-r", "-z", "--name-only", commit, "--"],
        )
        .await?;
        paths.extend(
            tree.split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_owned),
        );
    }
    let contains = |outer: &str, inner: &str| {
        inner == outer
            || inner
                .strip_prefix(outer)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    Ok(paths
        .into_iter()
        .filter(|path| {
            ignored
                .iter()
                .any(|ignored| contains(ignored, path) || contains(path, ignored))
        })
        .collect())
}

/// Point `reference` at `commit`, keeping it reachable.
pub async fn save(repo: &Path, reference: &str, commit: &str) -> Result<()> {
    run_isolated(repo, &["update-ref", reference, commit]).await?;
    Ok(())
}

/// Delete `reference` only while it names `commit`; `false` when it names
/// another commit by then. A missing ref counts as deleted.
pub async fn delete_at(repo: &Path, reference: &str, commit: &str) -> Result<bool> {
    let deleted = run_isolated(repo, &["update-ref", "-d", reference, commit]).await;
    if deleted.is_ok() || !super::ref_exists(repo, reference, super::isolated_command).await? {
        return Ok(true);
    }
    let current = super::verify_revision(repo, reference, run_isolated).await?;
    if current.trim() != commit {
        return Ok(false);
    }
    deleted.map(|_| true)
}

#[cfg(test)]
mod tests;
