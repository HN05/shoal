//! Undo a merged PR's merge on a worktree's branch with `git revert`.
use std::{ffi::OsString, path::Path};

use anyhow::{Context, Result, ensure};

use crate::{git, tools::Tool};

/// The merged PR's facts that tell what its merge left on the base branch.
pub struct Merged<'a> {
    /// The commit the merge left at the tip of the base branch.
    pub merge_commit: &'a str,
    /// The PR's own commits.
    pub commits: &'a [String],
    /// Where `origin` keeps the PR's head, when the forge keeps it.
    pub head_ref: Option<&'a str>,
}

/// What a PR's merge left on its base branch, and so what undoes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merge {
    /// A merge commit, reverted against its first parent.
    Commit(String),
    /// The commits the merge added, newest first: a squashed commit, or the
    /// PR's commits rebased or fast-forwarded onto the base.
    Commits(Vec<String>),
}

impl Merge {
    /// How the PR merged: a merge commit has several parents; a rebase
    /// leaves copies of the PR's commits, with their patches, as the base's
    /// latest first-parent commits; anything else is one squashed commit,
    /// whose revert undoes nothing else.
    pub async fn of(repo: &Path, merged: &Merged<'_>) -> Result<Self> {
        let merge_commit = merged.merge_commit;
        if parent_count(repo, merge_commit).await? > 1 {
            return Ok(Self::Commit(merge_commit.to_owned()));
        }
        if merged.commits.len() > 1 {
            ensure_present(repo, merged).await?;
            let latest = first_parent_commits(repo, merge_commit, merged.commits.len()).await?;
            if patch_ids(repo, &latest).await? == patch_ids(repo, merged.commits).await? {
                return Ok(Self::Commits(latest));
            }
        }
        Ok(Self::Commits(vec![merge_commit.to_owned()]))
    }

    /// The `git revert` command that applies the merge's inverse to the
    /// worktree without committing it.
    fn revert_command(&self) -> Vec<OsString> {
        let mut args = vec![
            Tool::Git.program(),
            "revert",
            "--no-commit",
            "--no-rerere-autoupdate",
        ];
        match self {
            Self::Commit(commit) => args.extend(["-m", "1", commit.as_str()]),
            Self::Commits(commits) => args.extend(commits.iter().map(String::as_str)),
        }
        args.into_iter().map(OsString::from).collect()
    }
}

/// What reverting a merge left in the worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reverted {
    Committed,
    /// Stopped with these paths in conflict, the revert abandoned.
    Conflicts(Vec<String>),
}

/// Revert `merge` in the worktree at `repo` and commit the result with
/// `message`, abandoning a revert that conflicts. `run` runs each command
/// that changes the worktree there and returns its exit code. Changes to
/// tracked files, such as a setup's, would join the revert, so they refuse
/// it, as does a revert that changes nothing.
pub async fn revert(
    repo: &Path,
    merge: &Merge,
    message: &str,
    run: impl AsyncFn(Vec<OsString>) -> Result<i32>,
) -> Result<Reverted> {
    ensure!(
        !has_tracked_changes(repo).await?,
        "the worktree has changes to tracked files, which the revert would include"
    );
    if run(merge.revert_command()).await? != 0 {
        let conflicts = conflicted_paths(repo).await?;
        ensure!(!conflicts.is_empty(), "git revert failed");
        ensure!(
            run(abort_command()).await? == 0,
            "git revert --abort failed"
        );
        return Ok(Reverted::Conflicts(conflicts));
    }
    ensure!(
        has_staged_changes(repo).await?,
        "the base branch no longer has the merge's changes, so there is nothing to revert"
    );
    ensure!(
        run(commit_command(message)).await? == 0,
        "git commit failed"
    );
    Ok(Reverted::Committed)
}

fn commit_command(message: &str) -> Vec<OsString> {
    [Tool::Git.program(), "commit", "--no-edit", "-m", message]
        .into_iter()
        .map(OsString::from)
        .collect()
}

fn abort_command() -> Vec<OsString> {
    [Tool::Git.program(), "revert", "--abort"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

async fn has_tracked_changes(repo: &Path) -> Result<bool> {
    let status = git::run(repo, &["status", "--porcelain", "--untracked-files=no"]).await?;
    Ok(!status.trim().is_empty())
}

async fn has_staged_changes(repo: &Path) -> Result<bool> {
    let staged = git::run(repo, &["diff", "--cached", "--name-only"]).await?;
    Ok(!staged.trim().is_empty())
}

/// The paths the worktree's index has in conflict.
async fn conflicted_paths(repo: &Path) -> Result<Vec<String>> {
    let paths = git::run(repo, &["diff", "--name-only", "--diff-filter=U", "-z"]).await?;
    Ok(paths
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The number of parents `commit` has.
async fn parent_count(repo: &Path, commit: &str) -> Result<usize> {
    let line = git::run(repo, &["rev-list", "--parents", "-n", "1", commit])
        .await
        .with_context(|| format!("the merge commit {commit} is not in the repository"))?;
    // The commit itself comes first.
    Ok(line.split_whitespace().count().saturating_sub(1))
}

/// Fetch the PR's head from `origin` when its commits are missing, as once
/// its branch is gone.
async fn ensure_present(repo: &Path, merged: &Merged<'_>) -> Result<()> {
    if has_all(repo, merged.commits).await? {
        return Ok(());
    }
    let head_ref = merged.head_ref.context(
        "the PR's commits are not in the repository, and the forge keeps no ref for them",
    )?;
    git::fetch::with_temporary_ref(repo, "revert", git::run, async |reference| {
        git::fetch::fetch_commit(repo, "origin", head_ref, reference, git::run).await
    })
    .await
    .context("could not fetch the PR's commits")?;
    ensure!(
        has_all(repo, merged.commits).await?,
        "the PR's head does not have the commits the forge lists for it"
    );
    Ok(())
}

async fn has_all(repo: &Path, commits: &[String]) -> Result<bool> {
    for commit in commits {
        if !git::has_commit(repo, commit).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The latest `count` first-parent commits up to `commit`, newest first.
async fn first_parent_commits(repo: &Path, commit: &str, count: usize) -> Result<Vec<String>> {
    let count = format!("--max-count={count}");
    let log = git::run(repo, &["rev-list", "--first-parent", &count, commit]).await?;
    Ok(log.lines().map(str::to_owned).collect())
}

/// The stable patch IDs of `commits`' changes, sorted, so copies of the same
/// changes compare equal whatever their order and commit IDs.
async fn patch_ids(repo: &Path, commits: &[String]) -> Result<Vec<String>> {
    let mut show = git::isolated_command(repo);
    show.args([
        "show",
        "--no-color",
        "--no-ext-diff",
        "--patch",
        "--format=commit %H",
    ])
    .args(commits);
    let patches = git::bounded(show).output().await?;
    let mut patch_id = git::isolated_command(repo);
    patch_id.args(["patch-id", "--stable"]);
    let ids = git::bounded(patch_id).input(patches).output().await?;
    Ok(sorted_patch_ids(&ids))
}

/// The patch IDs of `git patch-id` output, sorted.
fn sorted_patch_ids(output: &str) -> Vec<String> {
    let mut ids: Vec<String> = output
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect();
    ids.sort();
    ids
}

#[cfg(test)]
mod tests;
