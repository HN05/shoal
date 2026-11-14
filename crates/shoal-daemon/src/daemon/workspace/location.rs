//! Validate explicit worktree locations before claiming ownership.
use super::{
    Manager,
    paths::{canonical_with_missing_tail, contains_protected_directory},
};
use crate::{
    daemon::store,
    git,
    model::Repository,
    shoal_dirs::{Contents, ShoalDir},
};
use anyhow::{Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

impl Manager {
    /// Choose a new workspace's directory: a validated explicit path, or one
    /// named after `from`, such as its branch, inside the repository's
    /// reserved directory, resolved so overlap checks compare real
    /// locations. Any existing entry, including a dangling symlink, or a
    /// directory the daemon cannot write is refused.
    pub(super) async fn new_workspace_path(
        &self,
        repo: &Repository,
        from: &str,
        path: Option<PathBuf>,
    ) -> Result<PathBuf> {
        let path = match path {
            Some(path) => self.workspace_location(repo, &path).await?,
            None => {
                let directory = canonical_with_missing_tail(&self.workspaces_dir(repo).await?)?;
                directory.join(self.free_directory(from).await?)
            }
        };
        ensure_unused(&path)?;
        crate::daemon::access::ensure_writable(&path).await?;
        Ok(path)
    }

    /// Refuse what [`Self::new_workspace_path`] and recording the workspace
    /// would refuse for a workspace named after `from`, without reserving the
    /// repository's directory, which holds nothing before it is reserved.
    /// Without `from`, an explicit path may still reopen the workspace
    /// already there, so only its location is checked.
    pub(super) async fn check_new_workspace_path(
        &self,
        repo: &Repository,
        from: Option<&str>,
        path: Option<&Path>,
    ) -> Result<()> {
        let explicit = match path {
            Some(path) => Some(self.workspace_location(repo, path).await?),
            None => None,
        };
        let Some(from) = from else {
            return Ok(());
        };
        let path = match (explicit, &repo.workspaces_dir) {
            (Some(path), _) => path,
            (None, Some(directory)) => {
                canonical_with_missing_tail(directory)?.join(self.free_directory(from).await?)
            }
            (None, None) => return Ok(()),
        };
        ensure_unused(&path)?;
        self.store
            .run(move |db| store::ensure_path_unrecorded(db, &path))
            .await
    }

    /// The name of a new workspace's directory made from `from`, such as
    /// its branch, that no workspace's directory has.
    async fn free_directory(&self, from: &str) -> Result<String> {
        let base = crate::client::space_names::directory_name(from);
        self.store
            .run(move |db| store::free_directory(db, &base))
            .await
    }

    pub(super) async fn workspace_location(
        &self,
        repo: &Repository,
        path: &Path,
    ) -> Result<PathBuf> {
        ensure!(path.is_absolute(), "workspace path must be absolute");
        let path = canonical_with_missing_tail(path)?;
        ensure!(
            !contains_protected_directory(&path, &self.paths)?,
            "workspace path contains home or Shoal state"
        );
        ensure!(
            !path.starts_with(fs::canonicalize(&self.paths.state)?),
            "workspace path is inside Shoal state"
        );
        for other in self.repositories().await? {
            ensure!(
                !overlaps(&path, &other.path),
                "workspace path overlaps repository checkout {}",
                other.path.display()
            );
            if let Some(directory) = &other.workspaces_dir {
                ensure!(
                    !directory.starts_with(&path)
                        && (other.id == repo.id || !path.starts_with(directory))
                        && !Contents::ALL.into_iter().any(|contents| {
                            overlaps(&path, ShoalDir::new(directory, &other.id, contents).root())
                        })
                        && !overlaps(&path, &directory.join(crate::config::repo::LOCAL_DIR)),
                    "workspace path overlaps a reserved repository directory"
                );
            }
            let trees = match git::worktrees(&other.path, git::run).await {
                Ok(trees) => trees,
                Err(_) if other.id != repo.id => continue,
                Err(error) => return Err(error),
            };
            for tree in trees {
                // The candidate itself can be an explicitly adopted worktree.
                ensure!(
                    path == tree.path || !overlaps(&path, &tree.path),
                    "workspace path overlaps Git worktree {}",
                    tree.path.display()
                );
            }
        }
        // Also reject nesting in an unregistered checkout.
        let mut ancestor = path.parent();
        while let Some(parent) = ancestor {
            ensure!(
                !parent.join(".git").exists(),
                "workspace path is inside a Git checkout"
            );
            ancestor = parent.parent();
        }
        Ok(path)
    }
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Refuse any existing entry at a new workspace's path, including a dangling
/// symlink.
fn ensure_unused(path: &Path) -> Result<()> {
    ensure!(
        path.symlink_metadata()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "workspace path already exists: {}",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests;
