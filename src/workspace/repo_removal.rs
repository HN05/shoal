//! Explicit repository deletion, retaining ownership and progress for retries.
use super::{Manager, ownership::device_inode};
use crate::{
    git,
    model::{Repository, RepositoryRemoval, Workspace},
    removal::BranchChoice,
    state::WorkspaceState,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use std::{fs, path::Path};

/// Persisted progress of an interrupted removal.
struct Progress {
    /// Identity of the checkout directory when removal began; `None` when it
    /// was already missing.
    directory_id: Option<String>,
    /// Workspaces are gone and file deletion has started.
    deleting_files: bool,
}

impl Manager {
    pub(crate) async fn ensure_repository_available(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                ensure!(
                    !db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM repository_removals WHERE repository_id=?1)",
                        [id],
                        |r| r.get::<_, bool>(0)
                    )?,
                    "repository removal is incomplete; retry shoal repo rm --yes"
                );
                Ok(())
            })
            .await
    }

    pub async fn remove_repository(&self, selector: &str) -> Result<RepositoryRemoval> {
        // Registration cannot adopt or allocate a path during removal. The Git
        // gate also serializes with workspace creation, branch refresh, and reconciliation.
        let _registry = self.registry_gate.lock().await;
        let repo = self.repository(selector).await?;
        let gate = self.git_gate(&repo.id).await;
        let _git = gate.lock().await;
        let workspaces: Vec<_> = self
            .list_workspaces()
            .await?
            .into_iter()
            .filter(|w| w.repository_id == repo.id)
            .collect();
        self.check_repository_boundaries(&repo, &workspaces).await?;
        let progress = match self.removal_progress(&repo.id).await? {
            Some(progress) => progress,
            None => Progress {
                directory_id: directory_identity(&repo.path)?,
                deleting_files: false,
            },
        };
        let identity = progress.directory_id.as_deref();
        verify_directory(&repo.path, identity, progress.deleting_files)?;
        if progress.deleting_files {
            ensure!(
                workspaces.is_empty(),
                "repository has workspaces after file deletion began"
            );
        } else {
            self.remove_repository_workspaces(&repo, &workspaces, identity)
                .await?;
        }
        verify_directory(&repo.path, identity, true)?;
        delete_checkout(repo.path.clone()).await?;
        // Only an empty repository directory goes; anything else in it is the user's.
        if let Some(directory) = &repo.workspaces_dir {
            match fs::remove_dir(directory) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("delete {}", directory.display()));
                }
            }
        }
        let id = repo.id.clone();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                ensure!(
                    !tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM workspaces WHERE repository_id=?1)",
                        [&id],
                        |row| row.get::<_, bool>(0)
                    )?,
                    "repository still has workspaces"
                );
                tx.execute(
                    "DELETE FROM resource_pools WHERE scope=?1",
                    [format!("repo/{id}")],
                )?;
                ensure!(
                    tx.execute("DELETE FROM repositories WHERE id=?1", [&id])? == 1,
                    "repository registration disappeared during removal"
                );
                tx.commit()?;
                Ok(())
            })
            .await?;
        Ok(RepositoryRemoval {
            removed: true,
            repository_id: repo.id,
            path: repo.path,
            workspaces_removed: workspaces.len(),
        })
    }

    async fn removal_progress(&self, id: &str) -> Result<Option<Progress>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT directory_id,deleting_files FROM repository_removals WHERE repository_id=?1",
                        [id],
                        |row| {
                            Ok(Progress {
                                directory_id: row.get(0)?,
                                deleting_files: row.get(1)?,
                            })
                        },
                    )
                    .optional()?)
            })
            .await
    }

    /// Verify ownership of every workspace, record the removal, delete the
    /// workspaces, then mark the checkout ready for deletion.
    async fn remove_repository_workspaces(
        &self,
        repo: &Repository,
        workspaces: &[Workspace],
        identity: Option<&str>,
    ) -> Result<()> {
        if identity.is_some() {
            check_checkout(repo, workspaces).await?;
        } else {
            ensure!(
                workspaces.is_empty(),
                "repository checkout is missing; restore it before removing its workspaces"
            );
        }
        // Establish ownership for every workspace before deleting any of them.
        for workspace in workspaces {
            ensure!(
                matches!(
                    workspace.state,
                    WorkspaceState::Ready | WorkspaceState::Failed
                ),
                "workspace {} is busy",
                workspace.name
            );
            if workspace.path.exists() {
                self.verify_worktree(workspace).await?;
            } else {
                ensure!(
                    workspace.state == WorkspaceState::Failed,
                    "workspace {} is missing; reconcile it before repository removal",
                    workspace.name
                );
                ensure!(
                    self.missing_worktree(workspace).await?.is_none(),
                    "workspace {} was moved; restore it before repository removal",
                    workspace.name
                );
            }
        }
        let (id, recorded_identity) = (repo.id.clone(), identity.map(str::to_owned));
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT INTO repository_removals(repository_id,directory_id) VALUES (?1,?2) ON CONFLICT(repository_id) DO NOTHING",
                    params![id, recorded_identity],
                )?;
                Ok(())
            })
            .await?;
        for workspace in workspaces {
            self.remove_workspace(
                &workspace.id,
                BranchChoice::DeleteBranch,
                std::process::id(),
            )
            .await
            .with_context(|| {
                format!(
                    "remove workspace {}; repository retained for retry",
                    workspace.name
                )
            })?;
        }
        verify_directory(&repo.path, identity, false)?;
        if identity.is_some() {
            // External Git commands may have created a worktree during cleanup.
            check_checkout(repo, &[]).await?;
        }
        let id = repo.id.clone();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE repository_removals SET deleting_files=1 WHERE repository_id=?1",
                    [id],
                )?;
                Ok(())
            })
            .await
    }

    async fn check_repository_boundaries(
        &self,
        repo: &Repository,
        workspaces: &[Workspace],
    ) -> Result<()> {
        for protected in [&self.paths.home, &self.paths.state] {
            let protected = fs::canonicalize(protected)?;
            ensure!(
                !protected.starts_with(&repo.path),
                "repository directory contains Shoal state or the home directory; refusing deletion"
            );
        }
        for other in self.repositories().await? {
            ensure!(
                other.id == repo.id || !other.path.starts_with(&repo.path),
                "repository directory contains another registered repository: {}",
                other.path.display()
            );
        }
        for other in self.list_workspaces().await? {
            ensure!(
                workspaces.iter().any(|w| w.id == other.id) || !other.path.starts_with(&repo.path),
                "repository directory contains another repository's workspace: {}",
                other.name
            );
        }
        Ok(())
    }
}

async fn delete_checkout(path: std::path::PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        match fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("delete {}; retry shoal repo rm --yes", path.display())),
        }?;
        ensure!(
            directory_identity(&path)?.is_none(),
            "repository directory still exists; retry shoal repo rm --yes"
        );
        Ok(())
    })
    .await?
}

/// `device:inode` of a real, non-redirected directory; `None` when missing.
fn directory_identity(path: &Path) -> Result<Option<String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect repository directory"),
    };
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "repository path is not a directory or was replaced by a symlink"
    );
    ensure!(
        fs::canonicalize(path)? == path,
        "repository path was redirected; refusing deletion"
    );
    Ok(Some(device_inode(&metadata)))
}

fn verify_directory(path: &Path, expected: Option<&str>, allow_missing: bool) -> Result<()> {
    let actual = directory_identity(path)?;
    ensure!(
        actual.as_deref() == expected || (allow_missing && actual.is_none()),
        "repository directory was replaced during removal; refusing deletion"
    );
    Ok(())
}

/// The checkout must be a plain repository whose only linked worktrees are
/// the given Shoal workspaces.
async fn check_checkout(repo: &Repository, workspaces: &[Workspace]) -> Result<()> {
    let root = git::run(&repo.path, &["rev-parse", "--show-toplevel"]).await?;
    ensure!(
        fs::canonicalize(root.trim())? == repo.path,
        "repository path no longer points to its checkout root"
    );
    let git_dir = repo.path.join(".git");
    ensure!(
        directory_identity(&git_dir)?.is_some(),
        "repository Git directory is missing"
    );
    let common = git::run(
        &repo.path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    ensure!(
        fs::canonicalize(common.trim())? == git_dir,
        "repository uses external Git metadata; refusing deletion"
    );
    for tree in git::worktrees(&repo.path).await? {
        // Removing a directory outside Git leaves its registration behind.
        // Only ignore missing entries Git itself considers prunable; locked
        // worktrees may merely be on an unmounted disk. No global prune is needed
        // because successful repository deletion removes this metadata too.
        if tree.prunable && !tree.locked {
            match fs::symlink_metadata(&tree.path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("inspect linked worktree"),
                Ok(_) => {}
            }
        }
        let owned = workspaces.iter().any(|w| {
            w.path == tree.path
                || super::ownership::canonical_parent(&w.path).is_ok_and(|p| p == tree.path)
        });
        ensure!(
            tree.path == repo.path || owned,
            "repository has a worktree outside Shoal: {}; remove it first",
            tree.path.display()
        );
    }
    Ok(())
}
