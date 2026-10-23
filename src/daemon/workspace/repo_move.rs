//! Moving repository directories into a new `root_dir`. Intent is recorded
//! before anything moves, so an interrupted move finishes or rolls back.
use super::{
    GuardMode, Manager,
    identity::{directory_identity, has_directory_identity},
    paths::{is_real_directory, real_directory_identity},
    registry,
};
use crate::{
    daemon::{log, store},
    fsutil, git,
    model::{MovedRepository, Repository, Workspace},
    process, repository, seed,
    state::WorkspaceState,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// A recorded repository directory move.
struct Intent {
    repository_id: String,
    source: PathBuf,
    destination: PathBuf,
    /// The source directory's identity, which a rename carries to the
    /// destination.
    source_id: String,
    /// The identity of the reserved, empty destination.
    destination_id: String,
    /// The state each workspace of the repository returns to.
    states: BTreeMap<String, WorkspaceState>,
    /// The records name the destination.
    switched: bool,
}

impl Manager {
    /// The repositories [`Self::move_repositories`] would move into `root`, or
    /// the configured `root_dir`, refusing as it would before anything moves.
    pub async fn plan_repository_moves(&self, root: Option<&Path>) -> Result<Vec<Repository>> {
        let _registry = self.registry_gate.lock().await;
        Ok(self.movable_repositories(root).await?.1)
    }

    /// Move every repository directory outside the configured `root_dir` into
    /// it, with the workspaces and clone inside. Each repository must be idle;
    /// all are checked before the first moves.
    pub async fn move_repositories(&self) -> Result<Vec<MovedRepository>> {
        let _registry = self.registry_gate.lock().await;
        let (root, moving) = self.movable_repositories(None).await?;
        let mut moved = Vec::with_capacity(moving.len());
        for repo in moving {
            moved.push(self.move_repository(&repo, &root).await?);
        }
        Ok(moved)
    }

    /// The root and the repositories outside it, after finishing interrupted
    /// moves; refused unless every one of them can move. Callers hold the
    /// registry gate.
    async fn movable_repositories(
        &self,
        root: Option<&Path>,
    ) -> Result<(PathBuf, Vec<Repository>)> {
        self.finish_repository_moves().await?;
        let repositories = self.repositories().await?;
        let root = self.move_root(root, &repositories).await?;
        let moving: Vec<_> = repositories
            .into_iter()
            .filter(|repo| outside(repo, &root))
            .collect();
        for repo in &moving {
            self.check_movable(repo).await?;
        }
        Ok((root, moving))
    }

    /// Finish or roll back every move a stopped daemon or failed attempt left.
    pub async fn finish_repository_moves(&self) -> Result<()> {
        let pending: Vec<String> = self
            .store
            .run(|db| {
                Ok(db
                    .prepare("SELECT repository_id FROM repository_moves")?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await?;
        for id in pending {
            let _git = self.lock_repository_git(&id).await;
            self.finish_repository_move(&id)
                .await
                .context("finish an interrupted repository move")?;
        }
        Ok(())
    }

    /// `root`, or the configured one, refused inside a directory that would
    /// move into it or a workspace placed elsewhere.
    async fn move_root(&self, root: Option<&Path>, repositories: &[Repository]) -> Result<PathBuf> {
        let root = match root {
            Some(root) => root.to_owned(),
            None => self.config().root_dir(&self.paths)?,
        };
        let root = registry::prepare_root(
            &root,
            &fs::canonicalize(&self.paths.state)?,
            repositories.iter().map(|repo| repo.path.as_path()),
        )
        .await?;
        for directory in repositories
            .iter()
            .filter_map(|repo| repo.workspaces_dir.as_ref())
        {
            ensure!(
                !root.starts_with(directory),
                "root_dir {} is inside the repository directory {}",
                root.display(),
                directory.display()
            );
        }
        for workspace in self.list_workspaces().await? {
            ensure!(
                !root.starts_with(&workspace.path),
                "root_dir {} is inside the workspace {}",
                root.display(),
                workspace.name
            );
        }
        Ok(root)
    }

    /// Refuse a repository whose directory is missing or in use, or whose
    /// workspaces are busy or not verifiably its own.
    async fn check_movable(&self, repo: &Repository) -> Result<()> {
        self.ensure_repository_available(&repo.id).await?;
        let directory = repo
            .workspaces_dir
            .as_deref()
            .context("repository has no directory")?;
        ensure!(
            directory.is_dir() && !directory.is_symlink(),
            "repository directory {} is missing",
            directory.display()
        );
        self.ensure_only_its_own(repo, directory).await?;
        let id = repo.id.clone();
        self.store
            .run(move |db| movable_records(db, &id).map(drop))
            .await?;
        let processes = process::in_directory(directory).await?;
        ensure!(
            processes.is_empty(),
            "processes are using {}: {}; stop them or leave the directory first",
            directory.display(),
            processes.join(", ")
        );
        for workspace in self.repository_workspaces(&repo.id).await? {
            if workspace.path.exists() {
                self.verify_worktree(&workspace).await?;
            } else {
                ensure!(
                    self.missing_worktree(&workspace).await?.is_none(),
                    "workspace {} was moved; restore it before moving its repository",
                    workspace.name
                );
            }
        }
        Ok(())
    }

    /// Refuse a directory holding Shoal's state or another repository's
    /// checkout, directory or workspaces, whose records would not follow it.
    async fn ensure_only_its_own(&self, repo: &Repository, directory: &Path) -> Result<()> {
        ensure!(
            !fs::canonicalize(&self.paths.state)?.starts_with(directory),
            "{} contains Shoal's state directory",
            directory.display()
        );
        for other in self.repositories().await? {
            if other.id == repo.id {
                continue;
            }
            let owned =
                std::iter::once(other.path.as_path()).chain(other.workspaces_dir.as_deref());
            for path in owned {
                ensure!(
                    !path.starts_with(directory),
                    "{} contains {}, which belongs to the repository {}",
                    directory.display(),
                    path.display(),
                    repository::name(&other)
                );
            }
        }
        for workspace in self.list_workspaces().await? {
            ensure!(
                workspace.repository_id == repo.id || !workspace.path.starts_with(directory),
                "{} contains the workspace {} of another repository",
                directory.display(),
                workspace.name
            );
        }
        Ok(())
    }

    async fn move_repository(&self, repo: &Repository, root: &Path) -> Result<MovedRepository> {
        let _seed = self.stop_seed_build(&repo.id).await;
        let _git = self.lock_repository_git(&repo.id).await;
        let repo = self.repository(&repo.id).await?;
        self.check_movable(&repo).await?;
        let workspaces = self.repository_workspaces(&repo.id).await?;
        let _resources = self.exclude_resource_hooks(&workspaces).await?;
        let source = repo
            .workspaces_dir
            .clone()
            .context("repository has no directory")?;
        let destination = self.begin_move(&repo.id, root, &source).await?;
        let moved = async {
            fsutil::move_directory(&source, &destination).await?;
            self.switch_move(&repo.id, &source, &destination).await
        }
        .await;
        let finished = self.finish_repository_move(&repo.id).await;
        moved?;
        finished?;
        Ok(MovedRepository {
            repository: self.repository(&repo.id).await?,
            from: source,
            workspace_paths: workspaces.into_iter().map(|w| w.path).collect(),
        })
    }

    /// Keep permit hooks, which run in the workspaces, from starting.
    async fn exclude_resource_hooks(
        &self,
        workspaces: &[Workspace],
    ) -> Result<Vec<super::ResourceGuard>> {
        let mut guards = Vec::with_capacity(workspaces.len());
        for workspace in workspaces {
            guards.push(
                self.resource_guard(&workspace.id, GuardMode::Exclusive)
                    .await?,
            );
        }
        Ok(guards)
    }

    /// Reserve the destination in `root` and record the move, giving up the
    /// reservation when the move cannot be recorded.
    async fn begin_move(&self, id: &str, root: &Path, source: &Path) -> Result<PathBuf> {
        let destination = self.reserve_destination(root, source).await?;
        if let Err(error) = self.record_move(id, source, &destination).await {
            // The reservation belongs exclusively to this attempt.
            let _ = fs::remove_dir(&destination);
            return Err(error);
        }
        Ok(destination)
    }

    /// Reserve the source directory's name in `root`, suffixed when taken.
    async fn reserve_destination(&self, root: &Path, source: &Path) -> Result<PathBuf> {
        let name = source
            .file_name()
            .context("repository directory has no name")?
            .to_str()
            .context("repository directory is not UTF-8")?;
        registry::reserve_directory(root, name, &self.repositories().await?)
    }

    async fn repository_workspaces(&self, repository_id: &str) -> Result<Vec<Workspace>> {
        Ok(self
            .list_workspaces()
            .await?
            .into_iter()
            .filter(|workspace| workspace.repository_id == repository_id)
            .collect())
    }

    /// Reserve every workspace of the repository and record the intent, unless
    /// something started using the repository since it was checked.
    async fn record_move(&self, id: &str, source: &Path, destination: &Path) -> Result<()> {
        let id = id.to_owned();
        let source_id =
            real_directory_identity(source)?.context("repository directory is missing")?;
        let destination_id = directory_identity(destination)?;
        let source = utf8(source)?;
        let destination = utf8(destination)?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let states = movable_records(&tx, &id)?;
                tx.execute(
                    "INSERT INTO repository_moves
                        (repository_id,source,destination,source_id,destination_id,states)
                        VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        id,
                        source,
                        destination,
                        source_id,
                        destination_id,
                        serde_json::to_string(&states)?
                    ],
                )?;
                tx.execute(
                    "UPDATE workspaces SET state=?2 WHERE repository_id=?1",
                    params![id, WorkspaceState::Reconciling],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Point every record inside the moved directory at the destination.
    async fn switch_move(&self, id: &str, source: &Path, destination: &Path) -> Result<()> {
        let id = id.to_owned();
        let source = utf8(source)?;
        let destination = utf8(destination)?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let inside = |column: &str| {
                    format!("({column}=?2 OR substr({column},1,length(?2)+1)=?2||'/')")
                };
                let rebased = |column: &str| {
                    format!(
                        "CASE WHEN {} THEN ?3||substr({column},length(?2)+1) ELSE {column} END",
                        inside(column)
                    )
                };
                // A local registration's source names its checkout, perhaps
                // spelled differently; a URL source stays.
                tx.execute(
                    &format!(
                        "UPDATE repositories SET path={}, workspaces_dir={},
                            source=CASE WHEN substr(source,1,1)='/' AND {} THEN {} ELSE source END
                            WHERE id=?1",
                        rebased("path"),
                        rebased("workspaces_dir"),
                        inside("path"),
                        rebased("path")
                    ),
                    params![id, source, destination],
                )?;
                tx.execute(
                    &format!(
                        "UPDATE workspaces SET path={}, git_dir={} WHERE repository_id=?1",
                        rebased("path"),
                        rebased("git_dir")
                    ),
                    params![id, source, destination],
                )?;
                tx.execute(
                    "UPDATE repository_moves SET switched=1 WHERE repository_id=?1",
                    [&id],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Roll back a move that did not complete, or finish one whose directory
    /// reached its destination. Only directories with their recorded identity
    /// are deleted. Callers hold the repository's Git gate.
    async fn finish_repository_move(&self, id: &str) -> Result<()> {
        let Some(intent) = self.move_intent(id).await? else {
            return Ok(());
        };
        if !intent.switched && is_directory(&intent.source, &intent.source_id)? {
            // The rename did not happen or the copy is incomplete.
            remove_recorded(&intent.destination, &intent.destination_id)?;
            return self.close_move(&intent).await;
        }
        if !intent.switched {
            ensure!(
                is_directory(&intent.destination, &intent.source_id)?,
                "repository directory {} is neither in place nor at {}",
                intent.source.display(),
                intent.destination.display()
            );
            self.switch_move(id, &intent.source, &intent.destination)
                .await?;
        }
        // A renamed directory carries the source's identity; a copy fills the
        // reserved one. Without either, the original may be the only copy.
        ensure!(
            is_directory(&intent.destination, &intent.source_id)?
                || is_directory(&intent.destination, &intent.destination_id)?,
            "repository directory {} is unavailable; restore it, then run shoal repo migrate again",
            intent.destination.display()
        );
        // A copy's original goes first: copied worktrees' Git links still
        // resolve to it, and repair reconnects them only once it is gone.
        remove_recorded(&intent.source, &intent.source_id)?;
        self.reconnect_worktrees(id).await?;
        self.close_move(&intent).await
    }

    /// Repair Git's links between the checkout and its present worktrees,
    /// including the seed's, then verify each workspace still owns its
    /// worktree before recording its admin directory.
    async fn reconnect_worktrees(&self, id: &str) -> Result<()> {
        let repo = self.repository(id).await?;
        let present: Vec<_> = self
            .repository_workspaces(id)
            .await?
            .into_iter()
            .filter(|workspace| workspace.path.exists())
            .collect();
        let seed = seed::Layout::of(&repo).map(|layout| layout.worktree());
        let paths: Vec<_> = present
            .iter()
            .map(|w| w.path.clone())
            .chain(seed.filter(|worktree| worktree.exists()))
            .collect();
        git::repair_worktrees(&repo.path, &paths).await?;
        for workspace in &present {
            let verified = self.verify_and_record_worktree(workspace, true).await?;
            if let Err(error) = verified.identity {
                log!(
                    "worktree identity not recorded for {}: {error:#}",
                    workspace.name
                );
            }
        }
        Ok(())
    }

    async fn move_intent(&self, id: &str) -> Result<Option<Intent>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let row = db
                    .query_row(
                        "SELECT source,destination,source_id,destination_id,states,switched
                            FROM repository_moves
                            WHERE repository_id=?1",
                        [&id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get::<_, String>(4)?,
                                row.get(5)?,
                            ))
                        },
                    )
                    .optional()?;
                row.map(
                    |(source, destination, source_id, destination_id, states, switched)| {
                        Ok(Intent {
                            repository_id: id,
                            source: source.into(),
                            destination: destination.into(),
                            source_id,
                            destination_id,
                            states: serde_json::from_str(&states)?,
                            switched,
                        })
                    },
                )
                .transpose()
            })
            .await
    }

    /// Return each workspace to the state it had before the move, and forget
    /// the intent.
    async fn close_move(&self, intent: &Intent) -> Result<()> {
        let id = intent.repository_id.clone();
        let states = intent.states.clone();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                for (workspace, state) in states {
                    // A daemon restart quarantined it with an error that no
                    // longer applies once the move finished.
                    tx.execute(
                        "UPDATE workspaces SET state=?2,
                            error=CASE WHEN ?2=?3 THEN NULL ELSE error END
                            WHERE id=?1",
                        params![workspace, state, WorkspaceState::Ready],
                    )?;
                }
                tx.execute("DELETE FROM repository_moves WHERE repository_id=?1", [id])?;
                tx.commit()?;
                Ok(())
            })
            .await
    }
}

/// Whether the repository's directory lies outside `root`.
fn outside(repo: &Repository, root: &Path) -> bool {
    repo.workspaces_dir
        .as_deref()
        .is_some_and(|directory| directory.parent() != Some(root))
}

fn utf8(path: &Path) -> Result<String> {
    Ok(path
        .to_str()
        .context("repository directory is not UTF-8")?
        .to_owned())
}

/// Refuse a repository whose records show it in use: a lease that handed its
/// checkout to a caller, a command, agent, chat or queued launch, or a
/// workspace in another lifecycle operation. Returns each workspace's state.
fn movable_records(
    db: &rusqlite::Connection,
    id: &str,
) -> Result<BTreeMap<String, WorkspaceState>> {
    ensure!(
        !store::exists(
            db,
            "SELECT 1 FROM repository_resource_leases WHERE repository_id=?1",
            [id]
        )?,
        "the repository checkout is leased as a resource; release it first"
    );
    ensure!(
        !store::repository_has_executions(db, id)?,
        "a command or agent is running in a workspace of the repository; stop it first"
    );
    for (table, what) in [
        ("chats", "a chat is running in the repository"),
        (
            "agent_queue",
            "agent launches are queued for the repository",
        ),
    ] {
        ensure!(
            !store::exists(
                db,
                &format!("SELECT 1 FROM {table} WHERE repository_id=?1"),
                [id]
            )?,
            "{what}; wait for it to finish"
        );
    }
    let states = db
        .prepare("SELECT id,state FROM workspaces WHERE repository_id=?1")?
        .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<BTreeMap<String, WorkspaceState>>>()?;
    for (workspace, state) in &states {
        ensure!(
            matches!(state, WorkspaceState::Ready | WorkspaceState::Failed),
            "a workspace of the repository is {state}; wait for it to finish"
        );
        store::ensure_no_pending_rename(db, workspace)?;
    }
    Ok(states)
}

/// Whether `path` is still the real directory recorded as `identity`.
fn is_directory(path: &Path, identity: &str) -> Result<bool> {
    Ok(is_real_directory(path)? && has_directory_identity(path, identity)?)
}

/// Delete `path` when it is the directory recorded as `identity`, leaving a
/// replacement alone.
fn remove_recorded(path: &Path, identity: &str) -> Result<()> {
    if is_directory(path, identity)? {
        fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        daemon::workspace::{ExecutionKind, identity},
        test_support,
    };
    use std::sync::Arc;

    /// A manager with a cloned repository and one workspace under the default
    /// root, configured to move them into `<temp>/new`.
    async fn cloned() -> Result<(
        tempfile::TempDir,
        Arc<Manager>,
        Repository,
        Workspace,
        PathBuf,
    )> {
        let (temp, manager) = test_support::manager().await;
        let origin = test_support::repository(temp.path(), "origin");
        let repo = manager
            .register_repository(format!("file://{}", origin.display()).into(), None, None)
            .await?;
        let workspace = manager
            .create_workspace(&repo.id, "feature".into(), None, None, None)
            .await?;
        let root = fs::canonicalize(temp.path())?.join("new");
        test_support::configure(&manager, &format!("root_dir = {:?}\n", root)).await;
        Ok((temp, manager, repo, workspace, root))
    }

    #[tokio::test]
    async fn moves_the_clone_and_workspaces_into_the_new_root() -> Result<()> {
        let (_temp, manager, repo, workspace, root) = cloned().await?;
        let old = repo.workspaces_dir.clone().context("no directory")?;
        // The seed's locked worktree moves and reconnects too.
        test_support::git(
            &repo.path,
            &["worktree", "add", "--lock", "--detach", "../.seed/worktree"],
        );

        let moved = manager.move_repositories().await?;
        let [moved] = moved.as_slice() else {
            anyhow::bail!("expected one move: {moved:?}");
        };
        let directory = root.join("origin");
        assert_eq!(moved.from, old);
        assert_eq!(moved.workspace_paths, std::slice::from_ref(&workspace.path));
        assert_eq!(moved.repository.workspaces_dir.as_ref(), Some(&directory));
        assert_eq!(moved.repository.path, directory.join(".checkout"));
        assert!(!old.exists());

        let workspace = manager.workspace(&workspace.id).await?;
        assert_eq!(workspace.path, directory.join("feature"));
        assert_eq!(workspace.state, WorkspaceState::Ready);
        assert!(
            workspace
                .git_dir
                .as_ref()
                .is_some_and(|dir| dir.starts_with(&directory))
        );
        manager.verify_worktree(&workspace).await?;
        let trees = git::worktrees(&moved.repository.path, git::run).await?;
        for path in [&workspace.path, &directory.join(".seed/worktree")] {
            assert!(
                trees
                    .iter()
                    .any(|tree| tree.path == *path && !tree.prunable),
                "{} is not connected",
                path.display()
            );
        }

        // Moving again finds nothing outside the root, and new workspaces go there.
        assert!(manager.move_repositories().await?.is_empty());
        let next = manager
            .create_workspace(&repo.id, "next".into(), None, None, None)
            .await?;
        assert_eq!(next.path, directory.join("next"));
        Ok(())
    }

    #[tokio::test]
    async fn an_interrupted_rename_finishes_and_an_incomplete_copy_rolls_back() -> Result<()> {
        let (_temp, manager, repo, workspace, root) = cloned().await?;
        let source = repo.workspaces_dir.clone().context("no directory")?;
        let destination = root.join("origin");
        fs::create_dir_all(&destination)?;

        // A copy cut short leaves the source, so the move rolls back.
        manager.record_move(&repo.id, &source, &destination).await?;
        let error = manager
            .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
            .await
            .err()
            .context("removal reserved during a move")?;
        assert!(error.to_string().contains("repository move is incomplete"));
        let error = manager
            .start_chat(repo.id.clone().into(), "codex".into(), std::process::id())
            .await
            .err()
            .context("chat started during a move")?;
        assert!(error.to_string().contains("repository move is incomplete"));
        // Quarantine fails the reserved workspaces; setup must not revive one.
        manager.store.quarantine_interrupted_operations().await?;
        test_support::configure(
            &manager,
            &format!("root_dir = {root:?}\npre_setup_cmd = \"true\"\n"),
        )
        .await;
        let error = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Setup, None)
            .await
            .err()
            .context("setup started during a move")?;
        assert!(
            format!("{error:#}").contains("repository move is incomplete"),
            "{error:#}"
        );
        fs::write(destination.join("partial"), "")?;
        manager.finish_repository_moves().await?;
        assert!(!destination.exists());
        assert_eq!(manager.repository(&repo.id).await?.path, repo.path);
        assert_eq!(
            manager.workspace(&workspace.id).await?.state,
            WorkspaceState::Ready
        );

        // A completed rename finishes when the daemon starts again, leaving a
        // directory created at the old path since.
        fs::create_dir_all(&destination)?;
        manager.record_move(&repo.id, &source, &destination).await?;
        fs::rename(&source, &destination)?;
        fs::create_dir(&source)?;
        fs::write(source.join("unrelated"), "keep")?;
        manager.store.quarantine_interrupted_operations().await?;
        manager.finish_repository_moves().await?;
        let workspace = manager.workspace(&workspace.id).await?;
        assert_eq!(workspace.path, destination.join("feature"));
        assert_eq!(workspace.state, WorkspaceState::Ready);
        assert_eq!(workspace.error, None);
        manager.verify_worktree(&workspace).await?;
        assert_eq!(fs::read_to_string(source.join("unrelated"))?, "keep");
        Ok(())
    }

    #[tokio::test]
    async fn a_copied_move_reconnects_worktrees_before_deleting_the_original() -> Result<()> {
        let (_temp, manager, repo, workspace, root) = cloned().await?;
        let source = repo.workspaces_dir.clone().context("no directory")?;
        let destination = root.join("origin");
        fs::create_dir_all(&destination)?;

        // What a move to another filesystem leaves before the original goes.
        manager.record_move(&repo.id, &source, &destination).await?;
        fsutil::copy_directory(&source, &destination).await?;
        manager.switch_move(&repo.id, &source, &destination).await?;
        // An unavailable copy keeps the original.
        let away = root.join("unmounted");
        fs::rename(&destination, &away)?;
        assert!(manager.finish_repository_moves().await.is_err());
        assert!(source.join(".checkout").exists());
        fs::rename(&away, &destination)?;
        manager.finish_repository_moves().await?;

        assert!(!source.exists());
        let workspace = manager.workspace(&workspace.id).await?;
        assert_eq!(workspace.path, destination.join("feature"));
        assert!(
            workspace
                .git_dir
                .as_ref()
                .is_some_and(|dir| dir.starts_with(&destination))
        );
        manager.verify_worktree(&workspace).await?;
        assert_eq!(
            git::head_branch(&workspace.path, false, git::run)
                .await?
                .as_deref(),
            Some("feature")
        );
        let checkout = manager.repository(&repo.id).await?.path;
        let trees = git::worktrees(&checkout, git::run).await?;
        assert!(
            trees
                .iter()
                .any(|tree| tree.path == workspace.path && !tree.prunable)
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_replaced_worktree_keeps_the_move_unfinished() -> Result<()> {
        let (_temp, manager, repo, workspace, root) = cloned().await?;
        let source = repo.workspaces_dir.clone().context("no directory")?;
        let destination = root.join("origin");
        fs::create_dir_all(&destination)?;
        manager.record_move(&repo.id, &source, &destination).await?;
        fs::rename(&source, &destination)?;
        let admin = destination.join(".checkout/.git/worktrees/feature");
        identity::mark_owner(&admin, "another")?;

        let error = manager
            .finish_repository_moves()
            .await
            .err()
            .context("adopted a replaced worktree")?;
        assert!(format!("{error:#}").contains("another Shoal workspace"));
        assert!(manager.ensure_repository_available(&repo.id).await.is_err());
        assert_eq!(
            manager.workspace(&workspace.id).await?.state,
            WorkspaceState::Reconciling
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_checkout_placed_in_its_directory_moves_with_its_source() -> Result<()> {
        let (temp, manager) = test_support::manager().await;
        let directory = temp.path().join("shoal/project");
        fs::create_dir_all(&directory)?;
        let checkout = test_support::repository(&directory, "checkout");
        let repo = manager
            .register_repository(checkout.to_str().context("path")?.into(), None, None)
            .await?;
        assert_eq!(
            repo.workspaces_dir.as_deref(),
            Some(fs::canonicalize(&directory)?.as_path())
        );
        let root = fs::canonicalize(temp.path())?.join("new");
        test_support::configure(&manager, &format!("root_dir = {:?}\n", root)).await;

        // Another checkout registered inside the directory keeps it in place.
        let other = test_support::repository(&directory, "other");
        let other = manager
            .register_repository(other.to_str().context("path")?.into(), None, None)
            .await?;
        let error = manager
            .move_repositories()
            .await
            .err()
            .context("moved another repository's checkout")?;
        assert!(format!("{error:#}").contains("belongs to the repository"));
        manager.remove_repository(&other.id).await?;

        manager.move_repositories().await?;
        let moved = manager.repository(&repo.id).await?;
        let expected = root.join("project/checkout");
        assert_eq!(moved.path, expected);
        assert_eq!(Path::new(&moved.source), expected);
        Ok(())
    }

    #[tokio::test]
    async fn refuses_a_root_inside_a_repository_directory_or_workspace() -> Result<()> {
        let (temp, manager, repo, _workspace, _root) = cloned().await?;
        let placed = fs::canonicalize(temp.path())?.join("placed");
        manager
            .create_workspace(&repo.id, "placed".into(), None, None, Some(placed.clone()))
            .await?;
        for (inside, message) in [
            (
                repo.workspaces_dir
                    .clone()
                    .context("no directory")?
                    .join("nested"),
                "inside the repository directory",
            ),
            (placed.join("nested"), "inside the workspace placed"),
        ] {
            test_support::configure(&manager, &format!("root_dir = {:?}\n", inside)).await;
            let error = manager
                .move_repositories()
                .await
                .err()
                .context("moved into itself")?;
            assert!(error.to_string().contains(message), "{error:#}");
        }
        Ok(())
    }
}
