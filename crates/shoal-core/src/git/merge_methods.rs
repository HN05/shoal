//! Local squash and rebase merges: the commit each makes on the default
//! branch's commit, which the merge worker then fast-forwards the default
//! branch to. Neither changes a branch or a checkout, so one that does not
//! apply cleanly leaves everything as it was.
use std::path::Path;

use anyhow::{Context, Result};
use shoal_refs::CommitId;

use crate::git::{
    self,
    merge_tree::{self, Merged},
    run_isolated as git_run, scratch,
};

/// What squashing or rebasing a branch onto the default branch made.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    /// The commit the default branch moves to.
    Commit(CommitId),
    /// The default branch already has every change of the branch.
    Unchanged,
    /// The branch does not apply cleanly; what Git reported.
    Conflicts(String),
}

/// One commit on `previous` whose tree is the merge of `previous` and
/// `source`, by whoever merges, with `message`, or without one the message
/// plain Git gives a squash merge.
pub(crate) async fn squash(
    repo: &Path,
    previous: &CommitId,
    source: &CommitId,
    message: Option<&str>,
) -> Result<Applied> {
    let tree = match merge_tree::merge(repo, previous.as_str(), source.as_str()).await? {
        Merged::Tree(tree) => tree,
        Merged::Conflicts(report) => return Ok(Applied::Conflicts(report)),
    };
    if tree == merge_tree::tree_of(repo, previous.as_str()).await? {
        return Ok(Applied::Unchanged);
    }
    let message = match message {
        Some(message) => message.to_owned(),
        None => squash_message(repo, previous, source).await?,
    };
    let message = cleaned_message(repo, &message).await?;
    Ok(Applied::Commit(
        commit_tree(repo, &tree, previous, message).await?,
    ))
}

/// A commit of `tree` on `parent` with `message`, by whoever merges.
async fn commit_tree(
    repo: &Path,
    tree: &str,
    parent: &CommitId,
    message: String,
) -> Result<CommitId> {
    let mut command = git::isolated_command(repo);
    command.args(["commit-tree", tree, "-p", parent.as_str(), "-F", "-"]);
    let commit = git::bounded(command)
        .input(message)
        .output()
        .await
        .context("make the squash merge's commit")?;
    Ok(commit.trim().into())
}

/// The message `git merge --squash` gives the commit: a branch of one
/// commit keeps its message; one of several lists them all.
async fn squash_message(repo: &Path, previous: &CommitId, source: &CommitId) -> Result<String> {
    let range = format!("{previous}..{source}");
    let count = git_run(repo, &["rev-list", "--count", &range, "--"]).await?;
    if count.trim() == "1" {
        return git_run(repo, &["show", "-s", "--format=%B", source.as_str(), "--"]).await;
    }
    let log = git_run(
        repo,
        &[
            "log",
            "--no-decorate",
            "--no-color",
            "--no-mailmap",
            "--no-show-signature",
            "--date=default",
            "--format=medium",
            &range,
            "--",
        ],
    )
    .await?;
    Ok(format!("Squashed commit of the following:\n\n{log}"))
}

/// `message` as `git commit` cleans one it does not edit: without
/// trailing whitespace, repeated blank lines and blank lines around it.
async fn cleaned_message(repo: &Path, message: &str) -> Result<String> {
    let mut command = git::isolated_command(repo);
    command.arg("stripspace");
    git::bounded(command)
        .input(message)
        .output()
        .await
        .context("clean the squash merge's message")
}

/// The commits of `source` that `previous` lacks, copied onto `previous`
/// as `git rebase` copies them, keeping their authors, in a worktree of
/// its own; the last copy.
pub(crate) async fn rebase(repo: &Path, previous: &CommitId, source: &CommitId) -> Result<Applied> {
    let worktree = scratch::MERGE.add(repo, source).await?;
    let rebased = rebase_in(&worktree, previous).await;
    scratch::MERGE.remove(repo).await?;
    rebased
}

/// Rebase `worktree`'s HEAD onto `previous`.
async fn rebase_in(worktree: &Path, previous: &CommitId) -> Result<Applied> {
    let output = git::bounded(rebase_command(worktree, previous.as_str()))
        .capture()
        .await
        .context("rebase the branch")?;
    if !output.status.success() {
        return Ok(Applied::Conflicts(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout).trim_end(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    let commit = git::resolve_commit(worktree, "HEAD", git_run).await?;
    Ok(match commit == *previous {
        true => Applied::Unchanged,
        false => Applied::Commit(commit),
    })
}

/// `git rebase` of `worktree`'s HEAD onto `onto`, flattening merges and
/// leaving out changes `onto` has, without hooks or the user's settings
/// that would change what it copies.
fn rebase_command(worktree: &Path, onto: &str) -> tokio::process::Command {
    let mut command = git::isolated_command_without_submodules(worktree);
    command.args([
        "-c",
        "rebase.rebaseMerges=false",
        "rebase",
        "--no-autosquash",
        "--no-autostash",
        "--no-update-refs",
        onto,
    ]);
    // Nothing may wait on an editor.
    command.env("GIT_EDITOR", "true");
    command
}

/// The wrapper runs this only after stopping the merge process group: a
/// rebase it interrupted ran in a worktree of its own, which goes.
pub(crate) async fn remove_rebase(repo: &Path) -> Result<()> {
    scratch::MERGE
        .remove(repo)
        .await
        .context("could not remove the interrupted merge's rebase worktree")
}

#[cfg(test)]
mod tests;
