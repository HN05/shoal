//! Explicit repository deletion, retaining ownership and progress for retries.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use futures_util::FutureExt;
use rusqlite::{OptionalExtension, params};
use shoal_core::{config, git, repository::Selector};
use shoal_daemon_base::ownership::{
    has_directory_identity,
    paths::{contains_protected_directory, is_real_directory, matches_recorded_worktree_path},
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{Repository, RepositoryRemoval, Workspace},
    state::WorkspaceState,
};
use shoal_store as store;
use tokio::sync::{MutexGuard, OwnedMutexGuard};

use crate::{
    Repositories,
    daemon::Daemon,
    seed::state::{SeedPause, SeedStop},
};

mod files;

/// Persisted progress of an interrupted removal.
struct Progress {
    /// Identity of the checkout directory when removal began; `None` when it
    /// was already missing.
    directory_id: Option<String>,
    /// Workspaces are gone and file deletion has started.
    deleting_files: bool,
    workspace_paths: Vec<PathBuf>,
}

/// What a removal holds: the registry gate, so registration cannot adopt or
/// allocate a path meanwhile, the repository's fetch and Git gates, which
/// also serialize it with workspace creation, branch refresh and
/// reconciliation, and its stopped seed.
struct Held<'a> {
    _registry: MutexGuard<'a, ()>,
    _fetch: OwnedMutexGuard<()>,
    _git: OwnedMutexGuard<()>,
    _seed: SeedStop,
}

impl Repositories {
    /// Remove the repository `selector` names, with its spaces and the
    /// files Shoal keeps for it.
    pub async fn remove(
        &self,
        daemon: &dyn Daemon,
        selector: impl Into<Selector>,
    ) -> Result<RepositoryRemoval> {
        let selector = selector.into();
        // Removing a workspace hands over the agent of a chat that became it,
        // so the hand-overs come first, before the gates, as a conversion
        // takes them before the Git gate.
        let removal = async {
            let (repo, _held) = self.lock_for_removal(daemon, selector).await?;
            self.remove_locked(daemon, repo).await.map(Some)
        };
        let removed = daemon.hold_handovers(removal.boxed()).await?;
        removed.context("the repository's removal removed nothing")
    }

    /// Remove a contribution repository nothing of which is left, checked
    /// again under the gates workspace creation takes; `None` when something
    /// is, in its records or its checkout. The check of the records records
    /// the removal, which refuses new work from then on.
    pub(crate) async fn remove_finished_contribution(
        &self,
        daemon: &dyn Daemon,
        id: &RepositoryId,
    ) -> Result<Option<RepositoryRemoval>> {
        // The hand-overs first, as [`Self::remove`] takes them.
        let removal = async {
            let (repo, _held) = self.lock_for_removal(daemon, id.into()).await?;
            if !repo.contribution
                || self.checkout_has_work(&repo).await?
                || !self.begin_contribution_removal(&repo).await?
            {
                return Ok(None);
            }
            self.remove_locked(daemon, repo).await.map(Some)
        };
        daemon.hold_handovers(removal.boxed()).await
    }

    /// The repository `selector` names, with the gates its removal holds
    /// taken, its reviews stopped and the creations under way in it ended.
    async fn lock_for_removal(
        &self,
        daemon: &dyn Daemon,
        selector: Selector,
    ) -> Result<(Repository, Held<'_>)> {
        let registry = self.gates.lock_registry().await;
        let repo = self.repository(selector).await?;
        let seed = self.pause_seed_build(&repo.id).await;
        daemon.stop_reviews(&repo.id).await;
        let fetch = self.gates.lock_fetch(&repo.id).await;
        let git = self.gates.lock_git(&repo.id).await;
        let seed = self.settle_creations(&repo.id, seed).await?;
        let held = Held {
            _registry: registry,
            _fetch: fetch,
            _git: git,
            _seed: seed,
        };
        Ok((repo, held))
    }

    /// Wait for the creations under way in the repository `id`, whose Git
    /// gate the caller holds, to end, then stop its `paused` seed: a
    /// creation clones the seed's snapshot before it ends.
    async fn settle_creations(&self, id: &RepositoryId, paused: SeedPause) -> Result<SeedStop> {
        for workspace in self.repository_workspaces(id).await? {
            if workspace.state == WorkspaceState::Preparing {
                (self.gates)
                    .wait_for_creation_holding_git(&workspace.id)
                    .await;
            }
        }
        Ok(paused.stop().await)
    }

    /// Remove the repository; the caller holds its registry and Git gates.
    async fn remove_locked(
        &self,
        daemon: &dyn Daemon,
        repo: Repository,
    ) -> Result<RepositoryRemoval> {
        self.ensure_no_move_or_borrower(&repo.id).await?;
        let workspaces = self.repository_workspaces(&repo.id).await?;
        self.check_repository_boundaries(&repo, &workspaces).await?;
        // A run its review left may still have its reviewer's processes in
        // the directory `.reviews` would delete.
        daemon.ensure_no_review_runs_left(&repo.id).await?;
        // A gate run works in the checkout and writes into `.gate`.
        daemon.ensure_no_gate_runs_in(&repo.id).await?;
        daemon.delete_runner_of_removed(&repo).await?;
        let progress = self.removal_progress(&repo).await?;
        let workspace_paths = progress.with_paths_of(&workspaces);
        let identity = progress.directory_id.as_deref();
        verify_directory(&repo.path, identity, progress.deleting_files)?;
        if progress.deleting_files {
            ensure!(
                self.spaces_deepest_first(&repo.id).await?.is_empty(),
                "repository has spaces after file deletion began"
            );
        } else {
            let removal = self.remove_repository_spaces(
                daemon,
                &repo,
                &workspaces,
                identity,
                &workspace_paths,
            );
            removal.await?;
        }
        verify_directory(&repo.path, identity, true)?;
        self.delete_files(daemon, &repo).await?;
        // Before the registration goes, so a removal that stops meanwhile
        // cleans the entries when it is retried.
        let cleaned = daemon.trust_removed(&repo, workspace_paths.clone()).await;
        self.unregister(&repo.id).await?;
        if cleaned {
            daemon.forget_agent_environments(&repo).await;
        }
        Ok(RepositoryRemoval {
            removed: true,
            repository_id: repo.id,
            path: repo.path,
            workspaces_removed: workspaces.len(),
            workspace_paths,
        })
    }

    /// Refuse a repository an unfinished move may leave in either place, or
    /// whose checkout another repository's workspace borrows as a resource.
    async fn ensure_no_move_or_borrower(&self, id: &RepositoryId) -> Result<()> {
        let (id, records) = (id.to_owned(), self.records.clone());
        self.store
            .run(move |db| {
                store::ensure_repository_not_moving(db, &id)?;
                records.ensure_no_external_leases(db, &id)
            })
            .await
    }

    /// The progress an interrupted removal of `repo` recorded, or that of
    /// one starting now.
    async fn removal_progress(&self, repo: &Repository) -> Result<Progress> {
        let id = repo.id.clone();
        let recorded = self
            .store
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
            .await?;
        Ok(match recorded {
            Some(progress) => progress,
            None => Progress {
                directory_id: shoal_daemon_base::ownership::paths::real_directory_identity(
                    &repo.path,
                )?,
                deleting_files: false,
                workspace_paths: Vec::new(),
            },
        })
    }

    /// Verify ownership of every workspace, record the removal with
    /// `workspace_paths`, remove the repository's spaces, then mark the
    /// checkout ready for deletion.
    async fn remove_repository_spaces(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        workspaces: &[Workspace],
        identity: Option<&str>,
        workspace_paths: &[PathBuf],
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
            daemon.ensure_removable_workspace(workspace).await?;
        }
        (self.record_removal(&repo.id, identity, workspace_paths)).await?;
        for id in self.spaces_deepest_first(&repo.id).await? {
            // Another removal took it meanwhile, or that of one before it.
            let Some(space) = daemon.find_space(&id).await? else {
                continue;
            };
            daemon.remove_for_repository(&id).await.with_context(|| {
                format!(
                    "remove {} {}; repository retained for retry",
                    space.kind(),
                    space.name
                )
            })?;
        }
        verify_directory(&repo.path, identity, false)?;
        if identity.is_some() {
            // External Git commands may have created a worktree during cleanup.
            check_checkout(repo, &[]).await?;
        }
        self.record_files_deleting(&repo.id).await
    }

    /// The IDs of the spaces of the repository `id`, workspaces and chats,
    /// each before the spaces whose removals take it first, so that each
    /// goes through its own removal, which takes a workspace while it is
    /// being prepared, rather than through theirs.
    async fn spaces_deepest_first(&self, id: &RepositoryId) -> Result<Vec<SpaceId>> {
        let (id, records) = (id.to_owned(), self.records.clone());
        (self.store)
            .read(move |tx| {
                let mut ordered = store::repository_spaces(tx, &id)?
                    .into_iter()
                    .map(|space| Ok((records.removal_depth(tx, &space)?, space)))
                    .collect::<Result<Vec<_>>>()?;
                ordered.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
                Ok(ordered.into_iter().map(|(_, space)| space).collect())
            })
            .await
    }

    async fn check_repository_boundaries(
        &self,
        repo: &Repository,
        workspaces: &[Workspace],
    ) -> Result<()> {
        // Removal deletes the checkout and the local config's directory.
        let local = repo.workspaces_dir.join(config::repo::LOCAL_DIR);
        let other_repositories = self.list().await?;
        let other_workspaces = self.workspaces().await?;
        for deleted in [&repo.path, &local] {
            ensure!(
                !contains_protected_directory(deleted, &self.paths)?,
                "{} contains Shoal state or the home directory; refusing deletion",
                deleted.display()
            );
            for other in &other_repositories {
                for path in [&other.path, &other.workspaces_dir] {
                    ensure!(
                        other.id == repo.id || !path.starts_with(deleted),
                        "{} contains another registered repository: {}",
                        deleted.display(),
                        path.display()
                    );
                }
            }
            for other in &other_workspaces {
                ensure!(
                    workspaces.iter().any(|w| w.id == other.id) || !other.path.starts_with(deleted),
                    "{} contains another repository's workspace: {}",
                    deleted.display(),
                    other.name
                );
            }
        }
        Ok(())
    }

    async fn unregister(&self, id: &RepositoryId) -> Result<()> {
        let (id, records) = (id.to_owned(), self.records.clone());
        self.store
            .write(move |tx| {
                ensure!(
                    !store::repository_has_spaces(tx, &id)?,
                    "repository still has spaces"
                );
                records.forget_repository_pools(tx, &id)?;
                ensure!(
                    tx.execute("DELETE FROM repositories WHERE id=?1", [&id])? == 1,
                    "repository registration disappeared during removal"
                );
                Ok(())
            })
            .await
    }
}

impl Progress {
    /// The workspace paths recorded so far, with those of `workspaces`.
    fn with_paths_of(&self, workspaces: &[Workspace]) -> Vec<PathBuf> {
        let mut paths = self.workspace_paths.clone();
        for workspace in workspaces {
            let path =
                std::fs::canonicalize(&workspace.path).unwrap_or_else(|_| workspace.path.clone());
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        paths
    }
}

impl Repositories {
    /// Record the removal of the repository `id` with the checkout's
    /// `identity` and the paths of its workspaces, unless another
    /// repository's workspace borrows its checkout.
    async fn record_removal(
        &self,
        id: &RepositoryId,
        identity: Option<&str>,
        workspace_paths: &[PathBuf],
    ) -> Result<()> {
        let (id, recorded_identity, workspace_paths, records) = (
            id.clone(),
            identity.map(str::to_owned),
            serde_json::to_string(workspace_paths)?,
            self.records.clone(),
        );
        self.store
            .write(move |tx| {
                records.ensure_no_external_leases(tx, &id)?;
                tx.execute(
                    "INSERT INTO repository_removals(repository_id,directory_id,workspace_paths) VALUES (?1,?2,?3) ON CONFLICT(repository_id) DO UPDATE SET directory_id=excluded.directory_id, workspace_paths=excluded.workspace_paths",
                    params![id, recorded_identity, workspace_paths],
                )?;
                Ok(())
            })
            .await
    }

    /// Record that the removal of the repository `id` deletes its files
    /// from now on, its spaces gone.
    async fn record_files_deleting(&self, id: &RepositoryId) -> Result<()> {
        let id = id.clone();
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
    if let Some(problem) = shoal_core::repository::bare_flag_problem(&repo.path).await? {
        bail!("{problem}, then retry shoal repo rm --yes");
    }
    let root = git::worktree_root(&repo.path).await?;
    ensure!(
        root == repo.path,
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
            match std::fs::symlink_metadata(&tree.path) {
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
        let seed = crate::seed::Layout::of(repo).is_worktree(&tree.path);
        ensure!(
            tree.path == repo.path || owned || seed,
            "repository has a worktree outside Shoal: {}; remove it first",
            tree.path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
