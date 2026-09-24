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
                        && (other.id == repo.id || !path.starts_with(directory)),
                    "workspace path overlaps a reserved repository directory"
                );
            }
            let trees = match git::worktrees(&other.path).await {
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
