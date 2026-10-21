//! Validate explicit worktree locations before claiming ownership.
use super::{
    Manager,
    paths::{canonical_with_missing_tail, contains_protected_directory},
};
use crate::{git, model::Repository};
use anyhow::{Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

impl Manager {
    /// Choose a new workspace's directory: a validated explicit path, or its
    /// name inside the repository's reserved directory, resolved so overlap
    /// checks compare real locations. Any existing entry, including a
    /// dangling symlink, is refused.
    pub(super) async fn new_workspace_path(
        &self,
        repo: &Repository,
        name: &str,
        path: Option<PathBuf>,
    ) -> Result<PathBuf> {
        let path = match path {
            Some(path) => self.workspace_location(repo, &path).await?,
            None => canonical_with_missing_tail(&self.workspaces_dir(repo).await?)?.join(name),
        };
        ensure_unused(&path)?;
        Ok(path)
    }

    /// Refuse what [`Self::new_workspace_path`] would refuse without reserving
    /// the repository's directory, which holds nothing before it is reserved.
    pub(super) async fn check_new_workspace_path(
        &self,
        repo: &Repository,
        name: &str,
        path: Option<&Path>,
    ) -> Result<()> {
        let path = match (path, &repo.workspaces_dir) {
            (Some(path), _) => self.workspace_location(repo, path).await?,
            (None, Some(directory)) => canonical_with_missing_tail(directory)?.join(name),
            (None, None) => return Ok(()),
        };
        ensure_unused(&path)
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
                        && !overlaps(&path, crate::seed::Layout::new(directory, &other.id).root())
                        && !overlaps(
                            &path,
                            crate::caches::Layout::new(directory, &other.id).root(),
                        ),
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
mod tests {
    use crate::test_support::{manager, repository};

    #[tokio::test]
    async fn workspaces_stay_out_of_the_seed_and_cache_directories() {
        let (root, manager) = manager().await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let directory = repo.workspaces_dir.clone().unwrap();
        for reserved in [".seed/inside", ".caches", ".caches/shared/go"] {
            let Err(error) = manager
                .workspace_location(&repo, &directory.join(reserved))
                .await
            else {
                panic!("a reserved directory refuses the workspace");
            };
            assert!(
                error.to_string().contains("reserved repository directory"),
                "{reserved}: {error:#}"
            );
        }
        manager
            .workspace_location(&repo, &directory.join("feature"))
            .await
            .unwrap();
    }
}
