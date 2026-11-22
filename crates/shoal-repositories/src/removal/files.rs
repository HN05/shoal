//! The files a removed repository leaves: its seed, caches and the
//! directories Shoal keeps for it, the files other subsystems keep there,
//! its checkout, its local config and its directory.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use shoal_core::{
    caches, config,
    shoal_dirs::{self, Contents, ShoalDir},
};
use shoal_daemon_base::ownership::paths::is_real_directory;
use shoal_protocol::model::Repository;

use crate::{Repositories, daemon::Daemon};

impl Repositories {
    /// Delete what Shoal keeps of the removed `repo`, then its checkout,
    /// its local config and its directory, once nothing else is left there.
    pub(super) async fn delete_files(&self, daemon: &dyn Daemon, repo: &Repository) -> Result<()> {
        self.remove_seed(repo).await?;
        delete_caches(repo).await?;
        delete_shoal_dir(repo, Contents::TmpDirs).await?;
        delete_shoal_dir(repo, Contents::ReviewRuns).await?;
        delete_shoal_dir(repo, Contents::GateRuns).await?;
        delete_shoal_dir(repo, Contents::Imports).await?;
        delete_shoal_dir(repo, Contents::Fetched).await?;
        daemon.delete_kept_files(repo).await?;
        delete_checkout(repo.path.clone()).await?;
        delete_local_config(repo.workspaces_dir.clone()).await?;
        delete_empty_workspaces_dir(repo.workspaces_dir.clone()).await
    }
}

async fn delete_checkout(path: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        match fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("delete {}; retry shoal repo rm --yes", path.display())),
        }?;
        ensure!(
            !is_real_directory(&path)?,
            "repository directory still exists; retry shoal repo rm --yes"
        );
        Ok(())
    })
    .await?
}

async fn delete_caches(repo: &Repository) -> Result<()> {
    let caches = caches::Layout::of(repo);
    tokio::task::spawn_blocking(move || caches.remove()).await?
}

/// The local config and the scripts beside it belong to the registration.
/// Only a real directory in the recorded repository directory goes: the
/// recorded path is canonical, so a symlink at it or above it, or at `.shoal`,
/// may lead to files that are not the repository's. A Git checkout there is
/// never a local config.
async fn delete_local_config(directory: PathBuf) -> Result<()> {
    let local = directory.join(config::repo::LOCAL_DIR);
    tokio::task::spawn_blocking(move || {
        let real = |path: &Path| {
            fs::symlink_metadata(path)
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        };
        let unredirected = fs::canonicalize(&directory).is_ok_and(|path| path == directory);
        let checkout = fs::symlink_metadata(local.join(".git")).is_ok();
        if !unredirected || !real(&local) || checkout {
            return Ok(());
        }
        fs::remove_dir_all(&local).with_context(|| format!("delete {}", local.display()))
    })
    .await?
}

/// Delete the repository's directory of `contents`, which nothing outlives.
async fn delete_shoal_dir(repo: &Repository, contents: Contents) -> Result<()> {
    let dir = ShoalDir::of(repo, contents);
    tokio::task::spawn_blocking(move || dir.remove()).await?
}

async fn delete_empty_workspaces_dir(directory: PathBuf) -> Result<()> {
    // Only an empty repository directory goes; anything else in it is the user's.
    tokio::task::spawn_blocking(move || shoal_dirs::remove_if_empty(&directory)).await?
}

#[cfg(test)]
mod tests;
