//! Explicit repository deletion, retaining ownership and progress for retries.
use super::{
    Manager,
    identity::has_directory_identity,
    paths::{
        contains_protected_directory, is_real_directory, matches_recorded_worktree_path,
        real_directory_identity,
    },
};
use crate::{
    daemon::{resources::Scope, store},
    git,
    model::{Repository, RepositoryRemoval, Workspace},
    removal::{InspectionPolicy, UnsavedWork},
    state::WorkspaceState,
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use std::{collections::BTreeMap, fs, path::Path};

/// Persisted progress of an interrupted removal.
struct Progress {
    /// Identity of the checkout directory when removal began; `None` when it
    /// was already missing.
    directory_id: Option<String>,
    /// Workspaces are gone and file deletion has started.
    deleting_files: bool,
    workspace_paths: Vec<std::path::PathBuf>,
}

impl Manager {
    pub(crate) async fn ensure_repository_available(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                ensure!(
                    !store::exists(
                        db,
                        "SELECT 1 FROM repository_removals WHERE repository_id=?1",
                        [id]
                    )?,
                    "repository removal is incomplete; retry shoal repo rm --yes"
                );
                Ok(())
            })
            .await
    }

    pub async fn remove_repository(
        &self,
        selector: impl Into<crate::repository::Selector>,
    ) -> Result<RepositoryRemoval> {
        // Registration cannot adopt or allocate a path during removal. The Git
        // gate also serializes with workspace creation, branch refresh, and reconciliation.
        let _registry = self.registry_gate.lock().await;
        let repo = self.repository(selector).await?;
        let _seed = self.stop_seed_build(&repo.id).await;
        let _git = self.lock_repository_git(&repo.id).await;
        let id = repo.id.clone();
        self.store
            .run(move |db| crate::daemon::resources::ensure_no_external_leases(db, &id))
            .await?;
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
                directory_id: real_directory_identity(&repo.path)?,
                deleting_files: false,
                workspace_paths: Vec::new(),
            },
        };
        let mut workspace_paths = progress.workspace_paths.clone();
        for workspace in &workspaces {
            let path =
                std::fs::canonicalize(&workspace.path).unwrap_or_else(|_| workspace.path.clone());
            if !workspace_paths.contains(&path) {
                workspace_paths.push(path);
            }
        }
        let identity = progress.directory_id.as_deref();
        verify_directory(&repo.path, identity, progress.deleting_files)?;
        if progress.deleting_files {
            ensure!(
                workspaces.is_empty(),
                "repository has workspaces after file deletion began"
            );
        } else {
            self.remove_repository_workspaces(&repo, &workspaces, identity, &workspace_paths)
                .await?;
        }
        verify_directory(&repo.path, identity, true)?;
        self.remove_seed(&repo).await?;
        delete_caches(&repo).await?;
        delete_checkout(repo.path.clone()).await?;
        delete_empty_workspaces_dir(repo.workspaces_dir.clone()).await?;
        self.unregister_repository(&repo.id).await?;
        Ok(RepositoryRemoval {
            removed: true,
            repository_id: repo.id,
            path: repo.path,
            workspaces_removed: workspaces.len(),
            workspace_paths,
        })
    }

    /// Keep `environment`, the [`crate::env::AGENT_CONFIG_VARIABLES`] an
    /// agent of the workspace runs with, until the repository's removal
    /// cleans the agent's trust entries where they point. Other variables
    /// are ignored.
    pub async fn keep_agent_environment(
        &self,
        selector: &str,
        mut environment: BTreeMap<String, String>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        environment.retain(|name, _| crate::env::AGENT_CONFIG_VARIABLES.contains(&name.as_str()));
        if environment.is_empty() {
            return Ok(());
        }
        self.store_agent_environment(&workspace.repository_id, &environment)
            .await
    }

    /// Record `environment` for the repository once.
    async fn store_agent_environment(
        &self,
        repository_id: &str,
        environment: &BTreeMap<String, String>,
    ) -> Result<()> {
        let (repository_id, environment) = (
            repository_id.to_owned(),
            serde_json::to_string(environment)?,
        );
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT OR IGNORE INTO repository_agent_environments(repository_id,environment) VALUES (?1,?2)",
                    params![repository_id, environment],
                )?;
                Ok(())
            })
            .await
    }

    /// What [`Self::keep_agent_environment`] kept for the repository with
    /// `id`, which may be removed already.
    pub async fn agent_environments(&self, id: &str) -> Result<Vec<BTreeMap<String, String>>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let mut statement = db.prepare(
                    "SELECT environment FROM repository_agent_environments WHERE repository_id=?1 ORDER BY environment",
                )?;
                let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
                rows.map(|environment| Ok(serde_json::from_str(&environment?)?))
                    .collect()
            })
            .await
    }

    /// Forget what [`Self::keep_agent_environment`] kept for the removed
    /// repository with `id`, which no agent can add to again.
    pub async fn forget_agent_environments(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                ensure!(
                    !store::exists(db, "SELECT 1 FROM repositories WHERE id=?1", [&id])?,
                    "the repository is still registered"
                );
                db.execute(
                    "DELETE FROM repository_agent_environments WHERE repository_id=?1",
                    [id],
                )?;
                Ok(())
            })
            .await
    }

    async fn removal_progress(&self, id: &str) -> Result<Option<Progress>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT directory_id,deleting_files,workspace_paths FROM repository_removals WHERE repository_id=?1",
                        [id],
                        |row| {
                            let workspace_paths = row.get::<_, String>(2)?;
                            let workspace_paths = serde_json::from_str(&workspace_paths).map_err(
                                |error| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        2,
                                        rusqlite::types::Type::Text,
                                        Box::new(error),
                                    )
                                },
                            )?;
                            Ok(Progress {
                                directory_id: row.get(0)?,
                                deleting_files: row.get(1)?,
                                workspace_paths,
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
        workspace_paths: &[std::path::PathBuf],
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
        let (id, recorded_identity, workspace_paths) = (
            repo.id.clone(),
            identity.map(str::to_owned),
            serde_json::to_string(workspace_paths)?,
        );
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                crate::daemon::resources::ensure_no_external_leases(&tx, &id)?;
                tx.execute(
                    "INSERT INTO repository_removals(repository_id,directory_id,workspace_paths) VALUES (?1,?2,?3) ON CONFLICT(repository_id) DO UPDATE SET directory_id=excluded.directory_id, workspace_paths=excluded.workspace_paths",
                    params![id, recorded_identity, workspace_paths],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        for workspace in workspaces {
            self.remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
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
        ensure!(
            !contains_protected_directory(&repo.path, &self.paths)?,
            "repository directory contains Shoal state or the home directory; refusing deletion"
        );
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

    async fn unregister_repository(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                ensure!(
                    !store::repository_has_workspaces(&tx, &id)?,
                    "repository still has workspaces"
                );
                tx.execute(
                    "DELETE FROM resource_pools WHERE scope=?1",
                    [Scope::Repo(id.clone())],
                )?;
                ensure!(
                    tx.execute("DELETE FROM repositories WHERE id=?1", [&id])? == 1,
                    "repository registration disappeared during removal"
                );
                tx.commit()?;
                Ok(())
            })
            .await
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
            !is_real_directory(&path)?,
            "repository directory still exists; retry shoal repo rm --yes"
        );
        Ok(())
    })
    .await?
}

async fn delete_caches(repo: &Repository) -> Result<()> {
    let Some(caches) = crate::caches::Layout::of(repo) else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || caches.remove()).await?
}

async fn delete_empty_workspaces_dir(directory: Option<std::path::PathBuf>) -> Result<()> {
    let Some(directory) = directory else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || -> Result<()> {
        // Only an empty repository directory goes; anything else in it is the user's.
        match fs::remove_dir(&directory) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error).with_context(|| format!("delete {}", directory.display())),
        }
    })
    .await?
}

fn verify_directory(path: &Path, expected: Option<&str>, allow_missing: bool) -> Result<()> {
    let unchanged = match (is_real_directory(path)?, expected) {
        (false, expected) => allow_missing || expected.is_none(),
        (true, Some(recorded)) => has_directory_identity(path, recorded)?,
        (true, None) => false,
    };
    ensure!(
        unchanged,
        "repository directory was replaced during removal; refusing deletion"
    );
    Ok(())
}

/// The checkout must be a plain repository whose only linked worktrees are
/// the given Shoal workspaces.
async fn check_checkout(repo: &Repository, workspaces: &[Workspace]) -> Result<()> {
    if let Some(problem) = crate::repository::bare_flag_problem(&repo.path).await? {
        bail!("{problem}, then retry shoal repo rm --yes");
    }
    let root = git::worktree_root(&repo.path).await?;
    ensure!(
        fs::canonicalize(root.trim())? == repo.path,
        "repository path no longer points to its checkout root"
    );
    let git_dir = repo.path.join(".git");
    ensure!(
        is_real_directory(&git_dir)?,
        "repository Git directory is missing"
    );
    ensure!(
        git::common_dir(&repo.path).await? == git_dir,
        "repository uses external Git metadata; refusing deletion"
    );
    for tree in git::worktrees(&repo.path, git::run).await? {
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
        let mut owned = false;
        for workspace in workspaces {
            if matches_recorded_worktree_path(&workspace.path, &tree.path)? {
                owned = true;
                break;
            }
        }
        let seed = crate::seed::Layout::of(repo).is_some_and(|seed| seed.is_worktree(&tree.path));
        ensure!(
            tree.path == repo.path || owned || seed,
            "repository has a worktree outside Shoal: {}; remove it first",
            tree.path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::workspace::WorkspaceSource;
    use crate::test_support;
    use std::os::unix::fs::MetadataExt;

    #[tokio::test]
    async fn removal_waits_for_queued_workspace_creation() {
        let (root, manager) = test_support::manager().await;
        let checkout = test_support::repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();

        let gate = manager.lock_repository_git(&repo.id).await;
        let creating_manager = manager.clone();
        let creating_repo = repo.clone();
        let mut creating = Box::pin(async move {
            let _git = creating_manager
                .lock_repository_git(&creating_repo.id)
                .await;
            creating_manager
                .create_branch_workspace(
                    &creating_repo,
                    "racing".into(),
                    "racing".into(),
                    WorkspaceSource::New {
                        base: None,
                        parent: None,
                    },
                    None,
                    None,
                )
                .await
        });
        test_support::assert_pending(&mut creating).await;

        // Shared caches go with the repository directory.
        let caches = std::collections::BTreeMap::from([(
            "go".to_owned(),
            crate::config::repo::Cache {
                scope: crate::config::repo::CacheScope::Shared,
            },
        )]);
        crate::caches::directories(&repo, &caches).unwrap();
        let mut removing = Box::pin(manager.remove_repository(&repo.id));
        test_support::assert_pending(&mut removing).await;
        drop(gate);

        let workspace = creating.await.unwrap();
        let removal = removing.await.unwrap();
        assert_eq!(removal.workspaces_removed, 1);
        assert!(!workspace.path.exists());
        assert!(manager.repositories().await.unwrap().is_empty());
        assert!(manager.list_workspaces().await.unwrap().is_empty());
        assert!(!repo.path.exists());
        assert!(!repo.workspaces_dir.unwrap().exists());
    }

    #[tokio::test]
    async fn removal_names_a_bare_flag_on_a_checkout_with_a_working_tree() -> Result<()> {
        let (root, manager) = test_support::manager().await;
        let checkout = test_support::repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_string_lossy().into_owned().into(), None, None)
            .await?;
        test_support::git(&repo.path, &["config", "core.bare", "true"]);

        let error = manager
            .remove_repository(&repo.id)
            .await
            .err()
            .context("removal succeeded")?;
        let message = format!("{error:#}");
        assert!(message.contains("core.bare=true"), "{message}");
        assert!(message.contains("config core.bare false"), "{message}");
        assert!(repo.path.exists());

        test_support::git(&repo.path, &["config", "core.bare", "false"]);
        manager.remove_repository(&repo.id).await?;
        assert!(!repo.path.exists());
        Ok(())
    }

    #[test]
    fn recorded_identity_verifies_including_legacy_device_inode() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?;
        let directory = root.join("checkout");
        fs::create_dir(&directory)?;
        let metadata = fs::metadata(&directory)?;
        let legacy = format!("{}:{}", metadata.dev(), metadata.ino());
        let current = real_directory_identity(&directory)?.context("missing identity")?;
        for recorded in [current.as_str(), legacy.as_str()] {
            verify_directory(&directory, Some(recorded), false)?;
        }
        assert!(verify_directory(&directory, None, true).is_err());

        let moved = root.join("moved");
        fs::rename(&directory, &moved)?;
        fs::create_dir(&directory)?;
        for recorded in [current.as_str(), legacy.as_str()] {
            assert!(verify_directory(&directory, Some(recorded), true).is_err());
        }
        fs::remove_dir(&directory)?;
        verify_directory(&directory, Some(&current), true)?;
        verify_directory(&directory, None, false)?;
        assert!(verify_directory(&directory, Some(&current), false).is_err());
        Ok(())
    }
}
