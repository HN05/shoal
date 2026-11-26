//! A stand-in for the rest of the daemon, which the repositories' tests
//! build the service over. It has no coordinators, reviews, runner, agents
//! or forge.
use shoal_daemon_base::forge_access::ForgeAccess;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result, bail};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_core::shoal_dirs::ShoalDir;
use shoal_daemon_base::{
    alerts::{NewAlert, Sent},
    config_state::ConfigState,
    file_access::FileAccess,
    gates::Gates,
    notifications::{Coordinators, Notifications, Source},
    run_usage::RunUsage,
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{
        Base, Chat, Checkout, Repository, RepositoryRemoval, Space, SyncedRepository, Workspace,
    },
    paths::Paths,
    space_names::SpaceSelector,
    state::WorkspaceState,
};
use shoal_refs::BranchName;
use shoal_store::Store;
use shoal_store::test_support::seeds;

use crate::{
    Repositories,
    daemon::{
        Activity, Agents, Branches, Followers, Host, Occupants, Records, SeedAdmission, Spaces,
    },
    moves::{Running, chats::OutsideChat},
};

/// The service over a stand-in daemon.
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) repositories: Repositories,
    pub(crate) daemon: StandIn,
    pub(crate) file_access: Arc<FileAccess>,
    store: Store,
    config: Arc<ConfigState>,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        Self::in_root(tempfile::tempdir().unwrap()).await
    }

    /// [`Self::new`] in a home whose name Spotlight indexes, unlike
    /// `tempfile`'s `.tmp` directories, so the excluded root layout has
    /// something to change.
    pub(crate) async fn indexed() -> Self {
        let root = tempfile::Builder::new()
            .prefix("indexed")
            .tempdir_in("/tmp");
        Self::in_root(root.unwrap()).await
    }

    /// The service in a home under `root`.
    async fn in_root(root: tempfile::TempDir) -> Self {
        let paths = Paths::for_test(std::fs::canonicalize(root.path()).unwrap());
        std::fs::create_dir_all(&paths.state).unwrap();
        let store = shoal_store::test_support::unsynced_store(paths.state.join("state.sqlite3"))
            .await
            .unwrap();
        let config = shoal_core::test_support::global_config("");
        let gates = Arc::new(Gates::default());
        let config = Arc::new(ConfigState::new(config));
        let forges = ForgeAccess::new(store.clone(), gates.clone(), config.clone());
        let daemon = StandIn::default();
        let file_access = Arc::new(FileAccess::default());
        let repositories = Repositories::new(
            store.clone(),
            gates,
            config.clone(),
            Arc::new(Notifications::new(store.clone())),
            file_access.clone(),
            Arc::new(forges),
            paths,
            Arc::new(daemon.clone()),
        );
        Self {
            root,
            repositories,
            daemon,
            file_access,
            store,
            config,
        }
    }

    /// The configured `root_dir`.
    pub(crate) fn root_dir(&self) -> std::path::PathBuf {
        let repositories = &self.repositories;
        repositories.config().root_dir(&repositories.paths).unwrap()
    }

    /// Make `text` the global config.
    pub(crate) fn configure(&self, text: &str) {
        (self.config).publish(shoal_core::test_support::global_config(text));
    }

    /// Register the checkout or clone URL `source`.
    pub(crate) async fn register(&self, source: impl std::fmt::Display) -> Repository {
        let source = source.to_string().into();
        self.repositories
            .register(source, None, None)
            .await
            .unwrap()
    }

    /// Write the local config of the repository `id`.
    pub(crate) async fn write_local_config(&self, id: &RepositoryId, text: &str) {
        let path = self.repositories.local_config_path(id).await.unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Record a ready workspace `name` of `repo` in a new worktree on the
    /// branch `name` at `path`, or in its workspaces directory.
    pub(crate) async fn record_workspace(
        &self,
        repo: &Repository,
        name: &str,
        path: Option<PathBuf>,
    ) -> Workspace {
        let path = path.unwrap_or_else(|| repo.workspaces_dir.join(name));
        let worktree = Worktree::add(repo, name, &path);
        let workspace = self.insert_workspace(repo, name, worktree).await;
        let space = Space::from(workspace.clone());
        self.daemon.spaces.lock().unwrap().push(space);
        workspace
    }

    /// Record the ready workspace `name` of `repo` in `worktree`.
    async fn insert_workspace(
        &self,
        repo: &Repository,
        name: &str,
        worktree: Worktree,
    ) -> Workspace {
        let id = SpaceId::generate();
        let path = Path::new(&worktree.path);
        let mut workspace =
            shoal_protocol::test_support::workspace(id.as_str(), repo.id.as_str(), name, path);
        workspace.checkout = Checkout::Created {
            base: Base {
                commit: worktree.base.into(),
                reference: None,
            },
            git_dir: worktree.git_dir.into(),
        };
        let recorded = workspace.clone();
        workspace.number = (self.store)
            .write(move |tx| seeds::workspace(tx, &recorded))
            .await
            .unwrap();
        workspace
    }

    /// Record a chat running in the worktree of `workspace`.
    pub(crate) fn record_chat_in(&self, workspace: &Workspace) -> Chat {
        let chat: Chat = serde_json::from_value(serde_json::json!({
            "id": SpaceId::generate(),
            "workdir": {
                "kind": "worktree",
                "workspace": {
                    "id": workspace.id,
                    "name": workspace.name,
                    "branch": workspace.branch(),
                },
                "path": workspace.path,
            },
            "number": 2,
            "name": "chat",
            "repository_id": workspace.repository_id,
            "mode": "chat",
            "kind": "chat",
            "started_at": 0,
        }))
        .unwrap();
        self.daemon.spaces.lock().unwrap().push(chat.clone().into());
        chat
    }
}

/// A worktree Git added for a recorded workspace.
struct Worktree {
    path: String,
    git_dir: String,
    base: String,
}

impl Worktree {
    /// Add a worktree of `repo` at `path` on the new branch `branch`.
    fn add(repo: &Repository, branch: &str, path: &Path) -> Self {
        use shoal_core::test_support::git;
        let path = path.to_str().unwrap().to_owned();
        git(&repo.path, &["worktree", "add", "-q", "-b", branch, &path]);
        let read = |revision| {
            git(Path::new(&path), &["rev-parse", revision])
                .trim()
                .to_owned()
        };
        let (git_dir, base) = (read("--absolute-git-dir"), read("HEAD"));
        Self {
            path,
            git_dir,
            base,
        }
    }
}

/// A daemon whose spaces are those a test records, whose host is clear of
/// overload until a test strains it, and which admits every seed build at
/// once, keeping its runs, until a test stops it.
#[derive(Clone, Default)]
pub(crate) struct StandIn {
    spaces: Arc<Mutex<Vec<Space>>>,
    strained: Arc<AtomicBool>,
    pub(crate) seed_builds: SeedBuilds,
}

/// The seed builds the stand-in admitted: whether protection stops the
/// running one, and the runs they reported.
#[derive(Clone)]
pub(crate) struct SeedBuilds {
    pub(crate) stop: Arc<tokio::sync::watch::Sender<bool>>,
    pub(crate) runs: Arc<Mutex<Vec<RunUsage>>>,
}

impl Default for SeedBuilds {
    fn default() -> Self {
        Self {
            stop: Arc::new(tokio::sync::watch::channel(false).0),
            runs: Default::default(),
        }
    }
}

impl SeedAdmission for SeedBuilds {
    fn admitted(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn stopped(&self) -> tokio::sync::watch::Receiver<bool> {
        self.stop.subscribe()
    }

    fn using(&self, _: u64) {}

    fn runs_in(&self, _: u32) {}

    fn ran(&self, run: RunUsage) -> BoxFuture<'_, ()> {
        self.runs.lock().unwrap().push(run);
        Box::pin(async {})
    }
}

impl StandIn {
    /// Put the host over its overload thresholds, or clear it.
    pub(crate) fn strain(&self, strained: bool) {
        self.strained.store(strained, Ordering::Relaxed);
    }

    /// The recorded space `selector` names.
    fn recorded(&self, selector: &SpaceSelector) -> Option<Space> {
        let spaces = self.spaces.lock().unwrap();
        (spaces.iter())
            .find(|space| *selector == SpaceSelector::id(&space.id))
            .cloned()
    }
}

impl Spaces for StandIn {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        let space = self.recorded(selector);
        Box::pin(async move { space.with_context(|| format!("no space {selector}")) })
    }

    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        let space = self.recorded(&SpaceSelector::id(id));
        Box::pin(async { Ok(space) })
    }

    fn ensure_removable_workspace<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ensure_movable_workspace<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn record_moved_worktree<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn outside_dirs(&self, _: &Path) -> Vec<PathBuf> {
        Vec::new()
    }

    fn outside_chats_moving<'a>(
        &'a self,
        _: &'a Path,
        _: &'a Path,
        _: Running,
    ) -> BoxFuture<'a, Result<Vec<OutsideChat>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn move_outside_chats<'a>(
        &'a self,
        _: Vec<OutsideChat>,
        _: &'a Path,
        _: &'a Path,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn remove_for_repository<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { bail!("no space {id}") })
    }

    fn hold_handovers<'a>(
        &'a self,
        work: BoxFuture<'a, Result<Option<RepositoryRemoval>>>,
    ) -> BoxFuture<'a, Result<Option<RepositoryRemoval>>> {
        work
    }
}

impl Followers for StandIn {
    fn local_config_changed(&self) {}
}

impl Occupants for StandIn {
    fn ensure_no_reviews_in<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ensure_no_import_in(&self, _: ShoalDir) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn stop_reviews<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn ensure_no_review_runs_left<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ensure_no_gate_runs_in<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn delete_runner_of_removed<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn delete_kept_files<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl Agents for StandIn {
    fn trust_removed<'a>(&'a self, _: &'a Repository, _: Vec<PathBuf>) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn trust_moved<'a>(
        &'a self,
        _: &'a Repository,
        _: Vec<(PathBuf, PathBuf)>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn forget_agent_environments<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Coordinators for StandIn {
    fn tell(&self, _: NewAlert, _: bool, _: Source) -> BoxFuture<'_, Option<Sent>> {
        Box::pin(async { None })
    }
}

impl Branches for StandIn {
    fn fetch_default_remotes<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, Result<BranchName>> {
        Box::pin(async { bail!("no remotes") })
    }

    fn fast_forward_default<'a>(
        &'a self,
        _: &'a Repository,
        _: &'a BranchName,
    ) -> BoxFuture<'a, Result<SyncedRepository>> {
        Box::pin(async { bail!("no remotes") })
    }
}

impl Host for StandIn {
    fn clear_of_overload(&self, _: &[&Path]) -> bool {
        !self.strained.load(Ordering::Relaxed)
    }

    fn admit_seed_build<'a>(
        &'a self,
        _: &'a RepositoryId,
        _: bool,
    ) -> BoxFuture<'a, Result<Box<dyn SeedAdmission + 'a>>> {
        let admission: Box<dyn SeedAdmission> = Box::new(self.seed_builds.clone());
        Box::pin(async { Ok(admission) })
    }
}

impl Records for StandIn {
    fn workspaces(&self, db: &Connection) -> Result<Vec<Workspace>> {
        let query = format!(
            "SELECT {} FROM spaces JOIN workspaces USING(id) ORDER BY name",
            shoal_store::WORKSPACE_COLUMNS
        );
        let mut statement = db.prepare(&query)?;
        let rows = statement.query_map([], shoal_store::workspace)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn removal_depth(&self, _: &Connection, _: &SpaceId) -> Result<u32> {
        Ok(0)
    }

    fn ensure_no_external_leases(&self, _: &Connection, _: &RepositoryId) -> Result<()> {
        Ok(())
    }

    fn repository_in_use(&self, _: &Connection, _: &RepositoryId) -> Result<bool> {
        Ok(false)
    }

    fn repository_activity(&self, _: &Connection, _: &RepositoryId) -> Result<Activity> {
        Ok(Activity {
            checkout_leased: false,
            chats: false,
            launches: false,
            workspace_states: Default::default(),
        })
    }

    fn reconcile_workspaces(&self, _: &Transaction<'_>, _: &RepositoryId) -> Result<()> {
        Ok(())
    }

    fn restore_workspace_state(
        &self,
        _: &Transaction<'_>,
        _: &SpaceId,
        _: WorkspaceState,
    ) -> Result<()> {
        Ok(())
    }

    fn rebase_paths(
        &self,
        _: &Transaction<'_>,
        _: &RepositoryId,
        _: &Path,
        _: &Path,
    ) -> Result<()> {
        Ok(())
    }

    fn forget_repository_pools(&self, _: &Transaction<'_>, _: &RepositoryId) -> Result<()> {
        Ok(())
    }
}
