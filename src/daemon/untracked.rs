//! Git worktrees under a repository's Shoal directory that no workspace
//! owns, which the user may adopt.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::{
    daemon::{log, workspace::Manager},
    git,
    model::UntrackedWorktree,
};

/// What Shoal owns under a repository's workspace directory.
pub(super) struct OwnedWorktrees {
    pub(super) tracked: HashSet<PathBuf>,
    pub(super) seed: crate::seed::Layout,
    pub(super) verified_seed: bool,
}

impl OwnedWorktrees {
    /// Each Git worktree under `root`, other than the checkout and Shoal's
    /// seed, that no workspace owns.
    fn untracked<'a>(
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
                    && !(self.verified_seed && self.seed.is_worktree(&tree.path))
                    && !self.tracked.contains(&tree.path)
            })
            .map(|tree| tree.path.as_path())
    }

    /// Warnings for the seed path when Shoal did not create it and for each
    /// untracked Git worktree under `root`.
    pub(super) fn problems(
        &self,
        checkout: &Path,
        root: &Path,
        trees: &[git::Worktree],
    ) -> Vec<String> {
        let seed_path = self.seed.worktree();
        let foreign_seed = (seed_path.symlink_metadata().is_ok() && !self.verified_seed).then(|| {
            format!(
                "Seed path is not Shoal's seed worktree; Shoal will neither rebuild nor remove it: {}",
                seed_path.display()
            )
        });
        let untracked = self
            .untracked(checkout, root, trees)
            .map(|path| format!("Git worktree is not tracked by Shoal: {}", path.display()));
        foreign_seed.into_iter().chain(untracked).collect()
    }
}

impl Manager {
    /// The untracked Git worktrees under every repository's workspace
    /// directory. A repository whose Git gate is busy is left out, since its
    /// ownership is changing.
    pub async fn untracked_worktrees(&self) -> Result<Vec<UntrackedWorktree>> {
        let mut untracked = Vec::new();
        for repo in self.repositories().await? {
            let Some(root) = &repo.workspaces_dir else {
                continue;
            };
            let Some(_guard) = self.try_lock_repository_git(&repo.id).await else {
                continue;
            };
            let owned = self.owned_worktrees(&repo, root).await?;
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

    /// The caller holds the repository's Git gate so ownership stays current.
    pub(super) async fn owned_worktrees(
        &self,
        repo: &crate::model::Repository,
        root: &Path,
    ) -> Result<OwnedWorktrees> {
        let tracked = self
            .list_workspaces()
            .await?
            .into_iter()
            .map(|w| w.path)
            .collect();
        let seed = crate::seed::Layout::new(root, &repo.id);
        let verified_seed = crate::daemon::seed::is_seed_worktree(repo, &seed)
            .await
            .unwrap_or(false);
        Ok(OwnedWorktrees {
            tracked,
            seed,
            verified_seed,
        })
    }
}

#[cfg(test)]
mod tests;
