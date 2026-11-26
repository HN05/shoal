//! Git worktrees under a repository's Shoal directory that no workspace
//! owns, which the user may adopt.
use crate::Spaces;
use crate::daemon::Daemon;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::{
    git, log,
    model::UntrackedWorktree,
    shoal_dirs::{Contents, ShoalDir},
};

pub struct OwnedWorktrees {
    pub tracked: HashSet<PathBuf>,
    /// None unless the seed's worktree verifies as Shoal's.
    pub seed: Option<PathBuf>,
    /// None unless the directory of review runs is the one Shoal created.
    pub review_runs: Option<PathBuf>,
}

impl OwnedWorktrees {
    /// Leaves out the checkout, Shoal's seed and its review checkouts.
    pub fn untracked<'a>(
        &self,
        checkout: &Path,
        root: &Path,
        trees: &'a [git::Worktree],
    ) -> impl Iterator<Item = &'a Path> {
        trees
            .iter()
            .filter(move |tree| {
                tree.path.starts_with(root)
                    && tree.path != checkout
                    && !(self.seed.as_ref()).is_some_and(|seed| is_path(seed, &tree.path))
                    && !(self.review_runs.as_ref()).is_some_and(|runs| tree.path.starts_with(runs))
                    && !self.tracked.contains(&tree.path)
            })
            .map(|tree| tree.path.as_path())
    }
}

impl Spaces {
    /// Leaves out a repository whose Git gate is busy, since its ownership
    /// is changing.
    pub async fn untracked_worktrees(&self, daemon: &dyn Daemon) -> Result<Vec<UntrackedWorktree>> {
        let mut untracked = Vec::new();
        for repo in daemon.repositories().await? {
            let Some(_guard) = self.gates.try_lock_git(&repo.id) else {
                continue;
            };
            let root = &repo.workspaces_dir;
            let owned = self.owned_worktrees(daemon, &repo, root).await?;
            let trees = match git::worktrees(&repo.path, git::run).await {
                Ok(trees) => trees,
                Err(error) => {
                    log!(
                        "untracked worktrees of {} not listed: {error:#}",
                        repo.path.display()
                    );
                    continue;
                }
            };
            untracked.extend(owned.untracked(&repo.path, root, &trees).map(|path| {
                UntrackedWorktree {
                    repository_id: repo.id.clone(),
                    path: path.to_owned(),
                }
            }));
        }
        Ok(untracked)
    }

    /// The caller holds the repository's Git gate so ownership stays
    /// current.
    pub async fn owned_worktrees(
        &self,
        daemon: &dyn Daemon,
        repo: &crate::model::Repository,
        root: &Path,
    ) -> Result<OwnedWorktrees> {
        let tracked = self
            .list_workspaces()
            .await?
            .into_iter()
            .map(|w| w.part.path)
            .collect();
        let seed = daemon.verified_seed_worktree(repo, root).await;
        let review_runs = ShoalDir::new(root, &repo.id, Contents::ReviewRuns);
        Ok(OwnedWorktrees {
            tracked,
            seed,
            review_runs: review_runs.is_own().then(|| review_runs.root().to_owned()),
        })
    }
}

/// Compares as Git lists the worktree and after resolving links.
fn is_path(worktree: &Path, path: &Path) -> bool {
    path == worktree || std::fs::canonicalize(worktree).is_ok_and(|real| real == path)
}

#[cfg(test)]
mod tests;
