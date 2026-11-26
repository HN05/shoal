//! Local merges of a branch into another: the commit the merge method makes
//! on the target branch's commit, then the target branch fast-forwarded to
//! it, in the worktree that has it checked out when one does. Git moves that
//! worktree's files as a fast-forward moves them, keeping its other changes,
//! so the worktree need not be clean. A merge that does not apply cleanly
//! changes nothing.
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use shoal_refs::{BranchName, CommitId};

use crate::{
    forge::items::action::MergeMethod,
    git::{
        self,
        merge_methods::{self, Applied},
    },
};

/// The branch that merges, at its commit, by its method and message.
pub struct Source<'a> {
    pub branch: &'a BranchName,
    pub commit: &'a CommitId,
    pub method: MergeMethod,
    /// A merge commit's or squash's message; Git's own by default.
    pub message: Option<&'a str>,
}

/// The branch a merge goes into, at the commit it goes onto, and the
/// worktree that has it checked out, if one does.
pub struct Target<'a> {
    pub branch: &'a BranchName,
    pub commit: &'a CommitId,
    pub worktree: Option<&'a Path>,
}

#[derive(Debug)]
pub struct Merged {
    pub commit: CommitId,
    /// False when the target branch already had the source's commits.
    pub updated: bool,
    /// True when a merge fast-forwarded the target branch instead of
    /// making a merge commit.
    pub fast_forward: bool,
}

pub async fn merge(repo: &Path, source: &Source<'_>, target: &Target<'_>) -> Result<Merged> {
    let contained = git::is_ancestor(
        repo,
        source.commit.as_str(),
        target.commit.as_str(),
        git::isolated_command,
    )
    .await?;
    if contained {
        return Ok(Merged {
            commit: target.commit.clone(),
            updated: false,
            fast_forward: true,
        });
    }
    let applied = merged_commit(repo, source, target).await?;
    let fast_forward = matches!(&applied, Applied::Commit(commit) if commit == source.commit);
    let updated = advance(repo, source, target, applied).await?;
    let reference = git::local_ref(target.branch);
    Ok(Merged {
        commit: git::resolve_commit(repo, &reference, git::run_isolated).await?,
        updated,
        fast_forward: fast_forward && source.method == MergeMethod::Merge,
    })
}

/// The commit merging `source` into `target` makes, without changing a
/// branch or a checkout.
pub(crate) async fn merged_commit(
    repo: &Path,
    source: &Source<'_>,
    target: &Target<'_>,
) -> Result<Applied> {
    let (previous, commit) = (target.commit, source.commit);
    match source.method {
        MergeMethod::Merge => {
            let message = (source.message.map(str::to_owned))
                .unwrap_or_else(|| format!("Merge branch '{}'", source.branch));
            merge_methods::merge(repo, previous, commit, &message).await
        }
        MergeMethod::Squash => merge_methods::squash(repo, previous, commit, source.message).await,
        MergeMethod::Rebase => merge_methods::rebase(repo, previous, commit).await,
    }
}

/// Fast-forward `target` to the commit `applied` made, if it made one;
/// whether it did. One that does not apply cleanly changes nothing and names
/// what makes it apply.
pub(crate) async fn advance(
    repo: &Path,
    source: &Source<'_>,
    target: &Target<'_>,
    applied: Applied,
) -> Result<bool> {
    let commit = match applied {
        Applied::Commit(commit) => commit,
        Applied::Unchanged => return Ok(false),
        Applied::Conflicts(report) => bail!("{}\n{report}", unclean(source, target.branch)),
    };
    match target.worktree {
        Some(worktree) => fast_forward_worktree(worktree, target.branch, &commit).await?,
        None => {
            let reference = git::local_ref(target.branch);
            git::fast_forward_local(repo, commit.as_str(), &reference).await?;
        }
    }
    Ok(true)
}

/// Why merging `source` into `into` changed nothing, and what makes it
/// apply.
fn unclean(source: &Source<'_>, into: &BranchName) -> String {
    let branch = source.branch;
    let (merging, update) = match source.method {
        MergeMethod::Rebase => (format!("rebasing {branch} onto {into}"), "rebase"),
        MergeMethod::Squash => (format!("squashing {branch} into {into}"), "merge"),
        MergeMethod::Merge => (format!("merging {branch} into {into}"), "merge"),
    };
    format!(
        "{merging} does not apply cleanly, so nothing changed; run git {update} {into} in the workspace, resolve conflicts there, and retry"
    )
}

/// Fast-forward the worktree `worktree`, which must have `branch` checked
/// out, to `commit`, keeping its other changes.
async fn fast_forward_worktree(
    worktree: &Path,
    branch: &BranchName,
    commit: &CommitId,
) -> Result<()> {
    ensure!(
        git::head_branch(worktree).await?.as_ref() == Some(branch),
        "the checkout at {} is no longer on {branch}",
        worktree.display()
    );
    git::run_without_submodules(
        worktree,
        &[
            "merge",
            "--ff-only",
            "--no-squash",
            "--no-stat",
            "--no-autostash",
            "--no-overwrite-ignore",
            "--",
            commit.as_str(),
        ],
    )
    .await
    .with_context(|| format!("move {branch}'s checkout to {commit}"))
    .map(drop)
}

#[cfg(test)]
mod tests;
