//! Undo a merged PR's merge on a worktree's branch with `git revert`.
use std::path::Path;

use anyhow::{Context, Result, ensure};

use crate::git;

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
    /// How `merge_commit` merged a PR whose commits have `messages`: a merge
    /// commit has several parents; a rebase leaves the PR's commits, with
    /// their messages, as the base's latest first-parent commits; anything
    /// else is one squashed commit.
    pub async fn of(repo: &Path, merge_commit: &str, messages: &[String]) -> Result<Self> {
        if parent_count(repo, merge_commit).await? > 1 {
            return Ok(Self::Commit(merge_commit.to_owned()));
        }
        if messages.len() > 1 {
            let latest = first_parent_commits(repo, merge_commit, messages.len()).await?;
            if same_messages(latest.iter().map(|(_, message)| message.as_str()), messages) {
                return Ok(Self::Commits(
                    latest.into_iter().map(|(commit, _)| commit).collect(),
                ));
            }
        }
        Ok(Self::Commits(vec![merge_commit.to_owned()]))
    }

    /// The `git revert` arguments that apply the merge's inverse to the
    /// worktree without committing it.
    fn revert_args(&self) -> Vec<&str> {
        let mut args = vec!["revert", "--no-commit", "--no-rerere-autoupdate"];
        match self {
            Self::Commit(commit) => args.extend(["-m", "1", commit.as_str()]),
            Self::Commits(commits) => args.extend(commits.iter().map(String::as_str)),
        }
        args
    }
}

/// What reverting a merge left in the worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reverted {
    Committed,
    /// Stopped with these paths in conflict, the revert still under way.
    Conflicts(Vec<String>),
}

/// Revert `merge` in the worktree at `repo` and commit the result with
/// `message`, using the caller's Git configuration and hooks. Changes to
/// tracked files, such as a setup's, would join the revert, so they refuse
/// it, as does a revert that changes nothing.
pub async fn revert(repo: &Path, merge: &Merge, message: &str) -> Result<Reverted> {
    ensure!(
        !has_tracked_changes(repo).await?,
        "the worktree has changes to tracked files, which the revert would include"
    );
    if let Some(conflicts) = apply_inverse(repo, merge).await? {
        return Ok(Reverted::Conflicts(conflicts));
    }
    ensure!(
        has_staged_changes(repo).await?,
        "the base branch no longer has the merge's changes, so there is nothing to revert"
    );
    commit(repo, message).await?;
    Ok(Reverted::Committed)
}

/// Abandon a revert [`revert`] left in conflict, restoring the worktree.
pub async fn abort(repo: &Path) -> Result<()> {
    git::run(repo, &["revert", "--abort"]).await.map(drop)
}

/// The number of parents `commit` has.
async fn parent_count(repo: &Path, commit: &str) -> Result<usize> {
    let line = git::run(repo, &["rev-list", "--parents", "-n", "1", commit])
        .await
        .with_context(|| format!("the merge commit {commit} is not in the repository"))?;
    // The commit itself comes first.
    Ok(line.split_whitespace().count().saturating_sub(1))
}

/// Apply the inverse of `merge` to the index and worktree, returning the
/// paths in conflict when it stops on them.
async fn apply_inverse(repo: &Path, merge: &Merge) -> Result<Option<Vec<String>>> {
    let mut command = git::command(repo);
    command.args(merge.revert_args());
    let output = git::bounded(command).capture().await?;
    if output.status.success() {
        return Ok(None);
    }
    let conflicts = conflicted_paths(repo).await?;
    ensure!(
        !conflicts.is_empty(),
        "git revert failed: {}",
        crate::subprocess::diagnostic(&output.stderr)
    );
    Ok(Some(conflicts))
}

async fn has_tracked_changes(repo: &Path) -> Result<bool> {
    let status = git::run(repo, &["status", "--porcelain", "--untracked-files=no"]).await?;
    Ok(!status.trim().is_empty())
}

async fn has_staged_changes(repo: &Path) -> Result<bool> {
    let staged = git::run(repo, &["diff", "--cached", "--name-only"]).await?;
    Ok(!staged.trim().is_empty())
}

async fn commit(repo: &Path, message: &str) -> Result<()> {
    git::run(repo, &["commit", "--no-edit", "-m", message])
        .await
        .map(drop)
}

/// The latest `count` first-parent commits up to `commit`, newest first,
/// with their messages.
async fn first_parent_commits(
    repo: &Path,
    commit: &str,
    count: usize,
) -> Result<Vec<(String, String)>> {
    let count = format!("--max-count={count}");
    let log = git::run(
        repo,
        &[
            "log",
            "--first-parent",
            &count,
            "-z",
            "--format=%H%n%B",
            commit,
        ],
    )
    .await?;
    log_entries(&log)
}

/// The commits and messages of `git log -z --format=%H%n%B` output.
fn log_entries(log: &str) -> Result<Vec<(String, String)>> {
    log.split('\0')
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (commit, message) = entry
                .split_once('\n')
                .context("unexpected git log output")?;
            Ok((commit.to_owned(), message.to_owned()))
        })
        .collect()
}

/// Whether `found` are the messages `listed`, in any order, as Git and the
/// forge may end them differently.
fn same_messages<'a>(found: impl Iterator<Item = &'a str>, listed: &[String]) -> bool {
    let sorted = |messages: Vec<&str>| {
        let mut messages: Vec<String> = messages.into_iter().map(|m| m.trim().into()).collect();
        messages.sort();
        messages
    };
    sorted(found.collect()) == sorted(listed.iter().map(String::as_str).collect())
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

#[cfg(test)]
mod tests;
