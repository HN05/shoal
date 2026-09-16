//! Explicit repository deletion, retaining ownership and progress for retries.
use super::Manager;
use crate::{
    model::{Repository, RepositoryRemoval, Workspace},
    removal::Choice,
    state::WorkspaceState,
    worktrunk,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use std::{fs, os::unix::fs::MetadataExt, path::Path};

impl Manager {
    pub(super) async fn ensure_repository_available(&self, id: &str) -> Result<()> {
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

    pub async fn remove_repository(&self, selector: String) -> Result<RepositoryRemoval> {
        // Registration cannot adopt or allocate a path during removal. The Git
        // gate also serializes with workspace creation, pull and reconciliation.
        let _registry = self.repositories.lock().await;
        let repo = self.repository(&selector).await?;
        let gate = self.git_gate(&repo.id).await;
        let _git = gate.lock().await;
        let workspaces: Vec<_> = self
            .list()
            .await?
            .into_iter()
            .filter(|w| w.repository_id == repo.id)
            .collect();
        self.check_repository_boundaries(&repo, &workspaces).await?;
        let id = repo.id.clone();
        let progress: Option<(Option<String>, bool)> = self.store.run(move |db| {
            Ok(db.query_row("SELECT directory_id,deleting_files FROM repository_removals WHERE repository_id=?1", [id],
                |row| Ok((row.get(0)?, row.get(1)?))).optional()?)
        }).await?;
        let (identity, deleting_files) = match progress {
            Some(progress) => progress,
            None => (directory_identity(&repo.path)?, false),
        };
        verify_directory(&repo.path, identity.as_deref(), deleting_files)?;
        if deleting_files {
            ensure!(
                workspaces.is_empty(),
                "repository has workspaces after file deletion began"
            );
        } else {
            if identity.is_some() {
                check_checkout(&repo, &workspaces).await?;
            } else {
                ensure!(
                    workspaces.is_empty(),
                    "repository checkout is missing; restore it before removing its workspaces"
                );
            }
            // Establish ownership for every workspace before deleting any of them.
            for workspace in &workspaces {
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
            let (id, recorded_identity) = (repo.id.clone(), identity.clone());
            self.store.run(move |db| {
                db.execute("INSERT INTO repository_removals(repository_id,directory_id) VALUES (?1,?2) ON CONFLICT(repository_id) DO NOTHING", params![id, recorded_identity])?;
                Ok(())
            }).await?;
            for workspace in &workspaces {
                self.remove(
                    workspace.id.clone(),
                    Choice::DeleteBranch,
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
            verify_directory(&repo.path, identity.as_deref(), false)?;
            if identity.is_some() {
                // External Git commands may have created a worktree during cleanup.
                check_checkout(&repo, &[]).await?;
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
                .await?;
        }
        verify_directory(&repo.path, identity.as_deref(), true)?;
        let path = repo.path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            match fs::remove_dir_all(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error).with_context(|| {
                    format!("delete {}; retry shoal repo rm --yes", path.display())
                }),
            }
        })
        .await??;
        ensure!(
            directory_identity(&repo.path)?.is_none(),
            "repository directory still exists; retry shoal repo rm --yes"
        );
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
        for other in self.list().await? {
            ensure!(
                workspaces.iter().any(|w| w.id == other.id) || !other.path.starts_with(&repo.path),
                "repository directory contains another repository's workspace: {}",
                other.name
            );
        }
        Ok(())
    }
}

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
    Ok(Some(format!("{}:{}", metadata.dev(), metadata.ino())))
}

fn verify_directory(path: &Path, expected: Option<&str>, allow_missing: bool) -> Result<()> {
    let actual = directory_identity(path)?;
    ensure!(
        actual.as_deref() == expected || (allow_missing && actual.is_none()),
        "repository directory was replaced during removal; refusing deletion"
    );
    Ok(())
}

async fn check_checkout(repo: &Repository, workspaces: &[Workspace]) -> Result<()> {
    let root = worktrunk::git(&repo.path, &["rev-parse", "--show-toplevel"]).await?;
    ensure!(
        fs::canonicalize(root.trim())? == repo.path,
        "repository path no longer points to its checkout root"
    );
    let git_dir = repo.path.join(".git");
    ensure!(
        directory_identity(&git_dir)?.is_some(),
        "repository Git directory is missing"
    );
    let common = worktrunk::git(
        &repo.path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    ensure!(
        fs::canonicalize(common.trim())? == git_dir,
        "repository uses external Git metadata; refusing deletion"
    );
    let trees = worktrunk::git(&repo.path, &["worktree", "list", "--porcelain", "-z"]).await?;
    for record in trees.split("\0\0") {
        let fields: Vec<_> = record.split('\0').collect();
        let Some(path) = fields
            .iter()
            .find_map(|field| field.strip_prefix("worktree "))
        else {
            continue;
        };
        let path = Path::new(path);
        // Removing a directory outside Git leaves its registration behind.
        // Only ignore missing entries Git itself considers prunable; locked
        // worktrees may merely be on an unmounted disk. No global prune is needed
        // because successful repository deletion removes this metadata too.
        let prunable = fields
            .iter()
            .any(|f| *f == "prunable" || f.starts_with("prunable "));
        let locked = fields
            .iter()
            .any(|f| *f == "locked" || f.starts_with("locked "));
        if prunable && !locked {
            match fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("inspect linked worktree"),
                Ok(_) => {}
            }
        }
        ensure!(
            path == repo.path
                || workspaces.iter().any(|w| {
                    let canonical = w
                        .path
                        .parent()
                        .and_then(|parent| fs::canonicalize(parent).ok())
                        .zip(w.path.file_name())
                        .map(|(parent, name)| parent.join(name));
                    w.path == path || canonical.as_deref() == Some(path)
                }),
            "repository has a worktree outside Shoal: {}; remove it first",
            path.display()
        );
    }
    Ok(())
}
