//! The worktree a repository's seed builds in, which Shoal creates, marks
//! as its own and deletes with the seed.
use std::fs;

use anyhow::{Result, ensure};
use shoal_core::{fsutil::off_runtime, git};
use shoal_protocol::model::Repository;
use shoal_refs::CommitId;

use super::Layout;
use crate::Repositories;

impl Repositories {
    /// Delete the repository's seed. The caller holds a [`SeedStop`] and the
    /// repository's Git gate.
    pub(crate) async fn remove_seed(&self, repo: &Repository) -> Result<()> {
        let layout = Layout::of(repo);
        let worktree = layout.worktree();
        if worktree.symlink_metadata().is_ok() {
            ensure_seed_worktree(repo, &layout).await?;
            let outputs = layout.clone();
            off_runtime::run(move || {
                outputs.destroy_worktree_outputs();
                Ok(())
            })
            .await?;
            git::seed::remove_worktree(&repo.path, &worktree).await?;
        }
        tokio::task::spawn_blocking(move || layout.remove_owned()).await?
    }
}

pub(super) async fn check_out_seed(
    repo: &Repository,
    layout: &Layout,
    tip: &CommitId,
) -> Result<()> {
    layout.claim()?;
    let worktree = layout.worktree();
    if worktree.symlink_metadata().is_ok() {
        ensure_seed_worktree(repo, layout).await?;
        return git::seed::check_out(&worktree, tip.as_str()).await;
    }
    git::seed::add_worktree(&repo.path, &worktree, tip.as_str(), &layout.lock_reason()).await?;
    shoal_daemon_base::ownership::mark_owner(
        &git::git_dir(&worktree).await?,
        &layout.worktree_owner(),
    )
}

/// Refuse to change or delete a `.seed/worktree` unless it is the worktree
/// Shoal created for this repository's seed.
async fn ensure_seed_worktree(repo: &Repository, layout: &Layout) -> Result<()> {
    ensure!(
        is_seed_worktree(repo, layout).await?,
        "{} is not the seed worktree Shoal created; move it away to rebuild or remove the seed",
        layout.worktree().display()
    );
    Ok(())
}

/// Whether `.seed` is this repository's marked directory,
/// `.seed/worktree` is a real directory, Git run there reaches that
/// worktree of this repository, and its admin directory carries Shoal's
/// seed marker. Git keeps the marker across moves and drops it when the
/// worktree is re-created.
pub async fn is_seed_worktree(repo: &Repository, layout: &Layout) -> Result<bool> {
    let worktree = layout.worktree();
    let real = fs::symlink_metadata(&worktree).is_ok_and(|metadata| metadata.is_dir());
    if !real || layout.ensure_owned().is_err() {
        return Ok(false);
    }
    let Ok(dirs) = git::checkout_dirs(&worktree).await else {
        return Ok(false);
    };
    if fs::canonicalize(&worktree).ok().as_ref() != Some(&dirs.root)
        || dirs.common_dir != git::common_dir(&repo.path).await?
    {
        return Ok(false);
    }
    let owner = shoal_daemon_base::ownership::marked_owner(&dirs.git_dir)?;
    Ok(owner == Some(layout.worktree_owner()))
}
