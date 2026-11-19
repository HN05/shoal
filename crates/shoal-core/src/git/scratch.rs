//! Worktrees Shoal makes in a repository's Git directory for work that must
//! leave the default branch and its checkout as they were until it is done,
//! such as an unmerge's replay: one place per command, which the repository
//! Git gate gives one run of it at a time, so rollback finds what an
//! interrupted run left.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use shoal_refs::CommitId;

use crate::{
    fsutil::{self, off_runtime},
    git,
};

/// The scratch worktree of one command.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scratch {
    /// The directory in the Git directory that holds the worktree.
    directory: &'static str,
    /// The owner the directory is marked with.
    owner: &'static str,
    /// The command that uses it, as a refusal names it.
    command: &'static str,
}

/// Where `shoal merge` rebases a branch onto the default branch.
pub(crate) const MERGE: Scratch = Scratch {
    directory: "shoal-merge",
    owner: "shoal merge",
    command: "merge",
};

/// Where `shoal unmerge` replays the history after a merge.
pub(crate) const UNMERGE: Scratch = Scratch {
    directory: "shoal-unmerge",
    owner: "shoal unmerge",
    command: "unmerge",
};

impl Scratch {
    /// Create the worktree detached at `commit`, in a directory marked as
    /// Shoal's, after removing one an earlier run left; its path.
    pub(crate) async fn add(self, repo: &Path, commit: &CommitId) -> Result<PathBuf> {
        self.remove(repo).await?;
        let worktree = self.create_directory(repo).await?.join("replay");
        (add_replaced(repo, &worktree, commit).await)
            .with_context(|| format!("create the worktree shoal {} runs in", self.command))?;
        Ok(worktree)
    }

    /// Create the directory in `repo`'s Git directory that holds the
    /// worktree, marked as Shoal's; its path.
    async fn create_directory(self, repo: &Path) -> Result<PathBuf> {
        let directory = self.directory(repo).await?;
        let (marked, owner) = (directory.clone(), self.owner);
        off_runtime::run(move || Ok(fsutil::marked_dirs::create_marked_dir(&marked, owner)?))
            .await
            .with_context(|| format!("create {}", directory.display()))?;
        self.ensure_own(&directory).await?;
        Ok(directory)
    }

    /// Remove the worktree an earlier run left and Git's record of it, if
    /// any; a directory Shoal did not mark as its own is refused, not
    /// deleted.
    pub(crate) async fn remove(self, repo: &Path) -> Result<()> {
        let directory = self.directory(repo).await?;
        if !off_runtime::exists(&directory).await? {
            return Ok(());
        }
        self.ensure_own(&directory).await?;
        git::detached::remove(&directory.join("replay")).await
    }

    /// The directory in `repo`'s Git directory that holds the worktree.
    async fn directory(self, repo: &Path) -> Result<PathBuf> {
        git::git_path(repo, self.directory).await
    }

    /// Refuse a directory Shoal did not create.
    async fn ensure_own(self, directory: &Path) -> Result<()> {
        let (checked, owner) = (directory.to_owned(), self.owner);
        let marked =
            off_runtime::run(move || Ok(fsutil::marked_dirs::check_marked_dir(&checked, owner)?))
                .await
                .with_context(|| format!("read {}", directory.display()))?;
        ensure!(
            marked,
            "{} exists and Shoal did not create it, so shoal {command} does not work there; remove it to {command}",
            directory.display(),
            command = self.command
        );
        Ok(())
    }
}

/// Check out `commit` in a new linked worktree of `repo` at `worktree`, on
/// no branch. `--force` replaces the registration of a scratch worktree
/// whose directory is gone, and no other worktree's.
async fn add_replaced(repo: &Path, worktree: &Path, commit: &CommitId) -> Result<()> {
    git::worktree_add::run(|| {
        let mut command = git::isolated_command(repo);
        command
            .args(["worktree", "add", "--quiet", "--detach", "--force"])
            .arg(worktree)
            .arg(commit.as_str());
        command
    })
    .await
    .map(drop)
}

#[cfg(test)]
mod tests;
