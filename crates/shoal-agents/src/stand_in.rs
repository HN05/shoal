//! A stand-in for the rest of the daemon, which the agents' tests build the
//! service over: a home with its state directory under a temporary
//! directory, the global config a test gives, the workspaces a test
//! records, the agents a test registers with their executions, and alerts
//! that only note what they tell.
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension, Transaction};
use shoal_core::{
    config::{Config, Effective, repo::ConfigLayers},
    hooks::HookKind,
    paths::StateFiles,
    process::load::WorkLimit,
};
use shoal_daemon_base::{
    alerts::{About, AlertKind},
    callers::Caller,
    config_state::ConfigState,
    gates::Gates,
    jobs::{JobCost, JobStop},
    notifications::Notifications,
    signals::Signals,
};
use shoal_forge_types::items::{link::LinkKind, reference::ItemInput};
use shoal_ids::{
    execution_ids::ExecutionId, queue_entry_ids::QueueEntryId, repository_ids::RepositoryId,
    space_ids::SpaceId,
};
use shoal_protocol::{
    agent::Agent,
    approvals::ApprovalRequest,
    config::{
        modes::ModeName,
        permissions::{PermissionChange, PermissionsConfig},
    },
    demos::Launch,
    model::{
        ByKind, Chat, Completion, MergeOptions, MergePlan, OpenedWorkspace, PortReservation,
        PreparationWait, QueueEntry, QueueRequest, Relation, Repository, Space, SpaceKind,
        SpacePart, UnmergePlan, Workspace, WorkspaceUsage,
    },
    paths::Paths,
    progress::Progress,
    protocol::ScopeMemory,
    repository::Selector,
    scope_tokens::ScopeToken,
    space_names::{SpaceName, SpaceSelector},
    state::WorkspaceState,
};
use shoal_refs::CommitId;
use shoal_schedule::{
    priority::Priority,
    snapshot::Snapshot,
    stopping::{RunningDemo, RunningJob},
};
use shoal_store::lifecycle::Generation;
use tokio::sync::watch;

use crate::{
    Agents, capacity::Activity, daemon::*, execution_connection::RecordChange, model::QueuedJob,
};

/// The service over a stand-in daemon.
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) agents: Arc<Agents>,
    pub(crate) daemon: Arc<StandIn>,
    pub(crate) records: Arc<StandInRecords>,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        Self::with_config("").await
    }

    /// The service under the global config `text` adds to the tests' base.
    pub(crate) async fn with_config(text: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(std::fs::canonicalize(root.path()).unwrap());
        std::fs::create_dir_all(&paths.state).unwrap();
        let daemon = StandIn {
            config: shoal_core::test_support::global_config(text),
            ..StandIn::default()
        };
        let records = Arc::new(StandInRecords::default());
        let config = Arc::new(ConfigState::new(shoal_core::test_support::global_config(
            text,
        )));
        let agents = Arc::new(service(paths, config, records.clone()).await);
        daemon.use_store(&agents.store);
        Self {
            root,
            agents,
            daemon: Arc::new(daemon),
            records,
        }
    }

    /// The service a daemon that restarts builds over the same state,
    /// knowing only what it recorded there.
    pub(crate) async fn restart(self) -> Self {
        let paths = self.agents.paths.clone();
        let config = self.agents.config.clone();
        drop(self.agents);
        let agents = Arc::new(service(paths, config, self.records.clone()).await);
        self.daemon.use_store(&agents.store);
        Self { agents, ..self }
    }

    /// The repository `repo` at the root's `repo`, registered with the
    /// stand-in, which the fixture's workspaces belong to.
    pub(crate) fn repository(&self) -> Repository {
        let path = self.root.path().join("repo");
        std::fs::create_dir_all(&path).unwrap();
        let repository = Repository {
            id: "repo".into(),
            path: path.clone(),
            source: path.display().to_string(),
            last_used: 1,
            name: None,
            workspaces_dir: path.with_extension("workspaces"),
            contribution: false,
            shared_url: None,
        };
        let mut repositories = self.daemon.repositories.lock().unwrap();
        if !repositories.iter().any(|known| known.id == repository.id) {
            repositories.push(repository.clone());
        }
        repository
    }

    /// Record a ready workspace `name`, of the fixture's repository.
    pub(crate) async fn workspace(&self, name: &str) -> Workspace {
        self.repository();
        let path = self.root.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let workspace = shoal_protocol::test_support::workspace(name, "repo", name, &path);
        let id = workspace.id.clone();
        (self.agents.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO repositories(id,path,source,last_used)
                     VALUES ('repo','/repo','/repo',1)",
                    [],
                )?;
                tx.execute(
                    "INSERT INTO spaces(id,kind,repository_id,name)
                     VALUES (?1,'workspace','repo',?1)",
                    [&id],
                )?;
                tx.execute(
                    "INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
                     VALUES (?1,'/work/'||?1,?1,'ready','abc','/admin')",
                    [&id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let mut spaces = self.daemon.spaces.lock().unwrap();
        spaces.push(Space::from(workspace.clone()));
        workspace
    }

    /// Change the space `id` as `change` does, as the daemon reads it.
    pub(crate) fn update_space(&self, id: &SpaceId, change: impl FnOnce(&mut Space)) {
        let mut spaces = self.daemon.spaces.lock().unwrap();
        change(spaces.iter_mut().find(|space| space.id == *id).unwrap());
    }

    /// Record that the work of the space `id` is done, as `shoal done` does.
    pub(crate) async fn done(&self, id: &SpaceId) {
        let recorded = id.clone();
        (self.agents.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO space_completion(space_id,record) VALUES (?1,'{}')",
                    [&recorded],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        self.update_space(id, |space| {
            if let SpacePart::Workspace(workspace) = &mut space.part {
                workspace.done = true;
            }
        });
    }

    /// Record a chat `name`, without a repository, whose agent runs in an
    /// execution of the same name.
    pub(crate) async fn chat(&self, name: &str) -> SpaceId {
        let id = SpaceId::from(name);
        let recorded = id.clone();
        (self.agents.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO spaces(id,kind,name) VALUES (?1,'chat',?1)",
                    [&recorded],
                )?;
                tx.execute(
                    "INSERT INTO executions(id,space_id,state) VALUES (?1,?1,'running')",
                    [&recorded],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        id
    }

    /// Record a running execution `id` in `workspace` whose wrapper
    /// registered `agent`, when it gives one.
    pub(crate) async fn execution(
        &self,
        workspace: &Workspace,
        id: &str,
        agent: Option<Agent>,
    ) -> ExecutionId {
        let (space, execution) = (workspace.id.clone(), ExecutionId::from(id));
        let recorded = execution.clone();
        (self.agents.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO executions(id,space_id,state) VALUES (?1,?2,'running')",
                    rusqlite::params![recorded, space],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        if let Some(agent) = agent {
            (self.agents)
                .track_unconnected(&execution, &workspace.id, &workspace.name, agent)
                .await;
        }
        execution
    }
}

/// The service over the state directory of `paths`.
async fn service(paths: Paths, config: Arc<ConfigState>, records: Arc<StandInRecords>) -> Agents {
    let store = shoal_store::test_support::unsynced_store(paths.database())
        .await
        .unwrap();
    Agents::new(
        store.clone(),
        Arc::new(Gates::default()),
        config,
        Arc::new(Signals::default()),
        Arc::new(Notifications::new(store.clone())),
        Arc::default(),
        paths,
        records,
        Arc::new(StandInRemovals),
        Vec::new(),
    )
}

/// A daemon with the spaces and agents a test records.
#[derive(Default)]
pub(crate) struct StandIn {
    /// The config every space's settings resolve under.
    pub(crate) config: Config,
    pub(crate) spaces: Mutex<Vec<Space>>,
    /// The registered repositories.
    pub(crate) repositories: Mutex<Vec<Repository>>,
    /// What the alerts told, by kind.
    pub(crate) notified: Mutex<Vec<(AlertKind, String)>>,
    /// The failed state reports, each with the space it named.
    pub(crate) state_failures: Mutex<Vec<(SpaceSelector, String)>>,
    /// The service's store, which the workspaces' lifecycles live in.
    store: Mutex<Option<shoal_store::Store>>,
    /// Whether this machine hands its spaces over, as one joined to a hub.
    pub(crate) hands_over: std::sync::atomic::AtomicBool,
    /// The spaces each hand-over took.
    pub(crate) handings: Mutex<Vec<shoal_protocol::hub::packages::Handing>>,
    /// Where the next hand-over says the spaces went.
    pub(crate) handed: Mutex<Vec<(Space, shoal_protocol::hub::packages::Handed)>>,
    /// How often the machine told the hub it stays.
    pub(crate) stays: std::sync::atomic::AtomicUsize,
}

impl StandIn {
    fn find(&self, id: &SpaceId) -> Option<Space> {
        let spaces = self.spaces.lock().unwrap();
        spaces.iter().find(|space| space.id == *id).cloned()
    }

    /// Read and write the workspaces' lifecycles through `store`, the
    /// service's, as the daemon does through its own.
    fn use_store(&self, store: &shoal_store::Store) {
        *self.store.lock().unwrap() = Some(store.clone());
    }

    /// The service's store.
    fn store(&self) -> shoal_store::Store {
        let store = self.store.lock().unwrap();
        store
            .clone()
            .expect("the fixture gives the stand-in its store")
    }

    /// Where the lifecycle of the workspace `id` stands, while it exists.
    async fn lifecycle(&self, id: &SpaceId) -> Result<Option<shoal_store::lifecycle::Lifecycle>> {
        let id = id.to_owned();
        let store = self.store();
        store
            .run(move |db| shoal_store::lifecycle::lifecycle(db, &id))
            .await
    }

    /// Record `state` as the workspace `id`'s, with `error`.
    async fn record_state(
        &self,
        id: &SpaceId,
        state: WorkspaceState,
        error: Option<String>,
    ) -> Result<()> {
        let id = id.to_owned();
        let store = self.store();
        store
            .write(move |tx| {
                tx.execute(
                    "UPDATE workspaces SET state=?2,error=?3 WHERE id=?1",
                    rusqlite::params![id, state, error],
                )?;
                Ok(())
            })
            .await
    }

    fn selected(&self, selector: &SpaceSelector) -> Result<Space> {
        let id = (selector.given_id()).with_context(|| format!("{NOT_HERE}: {selector}"))?;
        self.find(id).with_context(|| format!("{NOT_HERE}: {id}"))
    }
}

/// What a test reached that the stand-in does not offer.
const NOT_HERE: &str = "the agents' stand-in has no such space";

impl Spaces for StandIn {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        let space = self.selected(selector);
        Box::pin(async { space })
    }

    fn space_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>> {
        let space = self.find(id).with_context(|| format!("{NOT_HERE}: {id}"));
        Box::pin(async { space })
    }

    fn space_exists<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        let exists = self.find(id).is_some();
        Box::pin(async move { Ok(exists) })
    }

    fn agent_of<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Agent>>> {
        Box::pin(async { Ok(None) })
    }

    fn space_settings<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.config.resolve(&ConfigLayers::default());
        Box::pin(async { settings })
    }

    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        let workspace = self.selected(selector).and_then(|space| {
            let ByKind::Workspace(workspace) = space.by_kind() else {
                anyhow::bail!("{selector} is a chat");
            };
            Ok(workspace)
        });
        Box::pin(async { workspace })
    }

    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>> {
        let spaces = self.spaces.lock().unwrap().clone();
        Box::pin(async { Ok(spaces) })
    }

    fn visible_spaces<'a>(&'a self, _: Option<&'a Caller>) -> BoxFuture<'a, Result<Vec<Space>>> {
        let spaces = self.spaces.lock().unwrap().clone();
        Box::pin(async { Ok(spaces) })
    }

    fn workspace_below<'a>(
        &'a self,
        _: &'a Caller,
        _: &'a mut SpaceSelector,
        _: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn reachable_queue<'a>(
        &'a self,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<QueueEntry>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn workspace_settings<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.config.resolve(&ConfigLayers::default());
        Box::pin(async { settings })
    }

    fn workspace_settings_with_templates<'a>(
        &'a self,
        _: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.config.resolve(&ConfigLayers::default());
        Box::pin(async { settings })
    }

    fn workspace_paths(&self) -> BoxFuture<'_, Result<Vec<PathBuf>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn chat_workspace<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: chat_workspace")) })
    }

    fn chat_by_id<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Chat>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn chat_repository<'a>(&'a self, _: &'a Chat) -> BoxFuture<'a, Result<Option<Repository>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn chat_checkout_path<'a>(&'a self, _: &'a Chat) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: chat_checkout_path")) })
    }

    fn chat_tmp_dir(&self, _: Option<&Repository>, _: &Chat) -> Result<Option<PathBuf>> {
        Ok(Default::default())
    }

    fn completion<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Completion>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn reopen_for_resume<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn space_blocked<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn space_progress<'a>(
        &'a self,
        _: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<Progress>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn launch_priority<'a>(
        &'a self,
        _: &'a QueueRequest,
        _: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Option<Priority>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn ensure_change_held<'a>(
        &'a self,
        _: Option<&'a Caller>,
        _: &'a Space,
        _: &'a PermissionChange,
        _: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ensure_agent_held<'a>(
        &'a self,
        _: Option<&'a Caller>,
        _: &'a SpaceSelector,
        _: &'a ModeName,
        _: &'a PermissionsConfig,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn verify_space<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn touch<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn run_agent_exit_hook<'a>(
        &'a self,
        _: &'a Space,
        _: &'a Agent,
        _: Option<i32>,
        _: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn workspace_hook<'a>(
        &'a self,
        _: &'a Workspace,
        _: HookKind,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn checkpoint_after_agent<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn workspace_usage(&self, _: &SpaceId) -> Option<WorkspaceUsage> {
        Default::default()
    }
}

impl Repositories for StandIn {
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        let repositories = self.repositories.lock().unwrap();
        let found = repositories.iter().find(|repository| repository.id == *id);
        let found = found.cloned().context(NOT_HERE);
        Box::pin(async { found })
    }

    fn selected_repository<'a>(&'a self, _: &'a Selector) -> BoxFuture<'a, Result<Repository>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: selected_repository")) })
    }

    fn repository_settings<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.config.resolve(&ConfigLayers::default());
        Box::pin(async { settings })
    }

    fn repository_layers<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<ConfigLayers>> {
        Box::pin(async { Ok(ConfigLayers::default()) })
    }
}

impl Alerts for StandIn {
    fn notify_space<'a>(
        &'a self,
        _: &'a SpaceName,
        _: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()> {
        self.notified.lock().unwrap().push((kind, message));
        Box::pin(async {})
    }

    fn report_state_failure<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        error: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        let mut failures = self.state_failures.lock().unwrap();
        failures.push((selector.clone(), error.to_owned()));
        Box::pin(async { Ok(()) })
    }

    fn notify<'a>(
        &'a self,
        _: Option<&'a SpaceName>,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()> {
        self.notified.lock().unwrap().push((kind, message));
        Box::pin(async {})
    }

    fn notify_about(&self, _: About, kind: AlertKind, message: String) -> BoxFuture<'_, ()> {
        self.notified.lock().unwrap().push((kind, message));
        Box::pin(async {})
    }

    fn tell_space<'a>(&'a self, _: &'a SpaceId, _: AlertKind, _: String) -> BoxFuture<'a, bool> {
        Box::pin(async { Default::default() })
    }

    fn notify_approval<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn ask_user_about<'a>(&'a self, _: &'a [ApprovalRequest]) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn tell_still_blocked<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Turns for StandIn {
    fn end_marked_turn<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: Option<&'a ExecutionId>,
        _: Option<String>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Lifecycles for StandIn {
    fn set_state<'a>(
        &'a self,
        id: &'a SpaceId,
        state: WorkspaceState,
        error: Option<String>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.record_state(id, state, error))
    }

    fn reserve_lifecycle<'a>(
        &'a self,
        id: &'a SpaceId,
        state: WorkspaceState,
    ) -> BoxFuture<'a, Result<WorkspaceState>> {
        let id = id.to_owned();
        let store = self.store();
        Box::pin(async move {
            store
                .write(move |tx| {
                    let lifecycle =
                        shoal_store::lifecycle::lifecycle(tx, &id)?.context(NOT_HERE)?;
                    let held = lifecycle.state;
                    anyhow::ensure!(
                        matches!(held, WorkspaceState::Ready | WorkspaceState::Failed),
                        "workspace is {held}"
                    );
                    tx.execute(
                        "UPDATE workspaces SET state=?2,error=NULL WHERE id=?1",
                        rusqlite::params![id, state],
                    )?;
                    Ok(held)
                })
                .await
        })
    }

    fn reserved_since<'a>(
        &'a self,
        id: &'a SpaceId,
        seen: Option<Generation>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            Ok(match self.lifecycle(id).await? {
                Some(lifecycle) => lifecycle.state.passes() || seen != Some(lifecycle.generation),
                None => seen.is_some(),
            })
        })
    }

    fn lifecycle_generation<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Generation>> {
        Box::pin(async move { Ok(self.lifecycle(id).await?.context(NOT_HERE)?.generation) })
    }

    fn check_lifecycle<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn await_reservation_end<'a>(
        &'a self,
        id: &'a SpaceId,
        wanted: &'a (dyn Fn() -> Result<()> + Send + Sync),
    ) -> BoxFuture<'a, Result<Generation>> {
        Box::pin(async move {
            let store = self.store();
            let mut changed = store.watch_changes();
            loop {
                wanted()?;
                match self.lifecycle(id).await?.context(NOT_HERE)? {
                    lifecycle if lifecycle.state.passes() => changed.changed().await?,
                    lifecycle => return Ok(lifecycle.generation),
                }
            }
        })
    }

    fn ensure_no_pending_rename<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn verify_worktree<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn reopen_workspace<'a>(
        &'a self,
        _: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<OpenedWorkspace>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: reopen_workspace")) })
    }

    fn refresh_admitted<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Operations for StandIn {
    fn start_waiting_preparations(self: Arc<Self>) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn tell_start_warnings<'a>(&'a self, _: &'a Space, _: &'a [String]) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn pushing_operations(&self) -> Box<dyn Fn(&str) -> bool + Send + Sync> {
        Box::new(|_| false)
    }

    fn operations_work_in(&self) -> HashSet<SpaceId> {
        Default::default()
    }

    fn operation_runs_in(&self, _: &SpaceId) -> bool {
        Default::default()
    }

    fn fakes_workers(&self) -> bool {
        Default::default()
    }
}

impl Merges for StandIn {
    fn prepare_merge<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: &'a MergeOptions,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<MergePlan>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: prepare_merge")) })
    }

    fn prepare_unmerge<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<UnmergePlan>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: prepare_unmerge")) })
    }

    fn authorize_scoped_merge<'a>(
        &'a self,
        _: &'a MergePlan,
        _: &'a Caller,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn authorize_scoped_unmerge<'a>(
        &'a self,
        _: &'a UnmergePlan,
        _: &'a Caller,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn update_merge_records<'a>(&'a self, _: &'a RecordChange) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn follow_pushed_merge_runs<'a>(&'a self, _: &'a RecordChange) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Demos for StandIn {
    fn running_demos(&self) -> BoxFuture<'_, Result<Vec<RunningDemo>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn viewer_group<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, Result<Option<u32>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn retire_execution_viewer<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn reported_end<'a>(
        &'a self,
        _: &'a ExecutionId,
        _: Option<i32>,
    ) -> BoxFuture<'a, Option<i32>> {
        Box::pin(async { Default::default() })
    }

    fn unrun_demo_launch<'a>(
        &'a self,
        _: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn restart_demo_command<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn viewer_spawn_lock(&self, _: &ExecutionId) -> Arc<tokio::sync::Mutex<()>> {
        Default::default()
    }

    fn end_viewer(&self, _: &ExecutionId) {}
}

impl Links for StandIn {
    fn settle_agent(&self, _: &SpaceId) {}

    fn agent_updates_recorded<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn updates_waiting<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn restart_watches<'a>(
        &'a self,
        _: &'a SpaceId,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn current_head<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<CommitId>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: current_head")) })
    }

    fn link_item<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: LinkKind,
        _: &'a ItemInput,
        _: Relation,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl Runner for StandIn {
    fn job_executions(&self) -> BoxFuture<'_, Result<HashMap<SpaceId, ExecutionId>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn running_jobs(&self) -> BoxFuture<'_, Result<Vec<RunningJob>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn runs_job<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, bool> {
        Box::pin(async { Default::default() })
    }

    fn wake_runner_for_ended_job<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn job_workspaces(&self) -> BoxFuture<'_, HashSet<SpaceId>> {
        Box::pin(async { Default::default() })
    }

    fn stop_jobs_to_rerun(&self, _: JobStop) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn cancel_job_in<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: JobStop,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn stop_job_for_overload<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'static str,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn wake_jobs(&self) {}

    fn job_permits_busy(&self, _: &RepositoryId) -> bool {
        Default::default()
    }

    fn job_cost<'a>(
        &'a self,
        _: &'a RepositoryId,
        _: &'a QueuedJob,
    ) -> BoxFuture<'a, Result<JobCost>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: job_cost")) })
    }

    fn pass_over_job<'a>(
        &'a self,
        _: &'a QueueEntryId,
        _: &'a RepositoryId,
        _: &'a QueuedJob,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn job_activities(&self) -> BoxFuture<'_, Result<Vec<Activity>>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl Leases for StandIn {
    fn release_execution_leases<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn release_agent_leases<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn release_ended_leases_of<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn give_demo_its_leases<'a>(
        &'a self,
        _: &'a ExecutionId,
        _: &'a SpaceId,
        _: &'a shoal_protocol::demos::Launch,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn reserved_ports<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Vec<PortReservation>>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl Reviews for StandIn {
    fn queue_unreviewed(&self, error: anyhow::Error) -> BoxFuture<'_, anyhow::Error> {
        Box::pin(async { error })
    }
}

impl Conversions for StandIn {
    fn stop_converting_agent<'a>(&'a self, _: &'a SpaceId, _: bool) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn ensure_takes_agents<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn hand_over_before_removal<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl Memory for StandIn {
    fn record_scope_memory<'a>(&'a self, _: &'a ExecutionId, _: ScopeMemory) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn work_limit(&self) -> Option<WorkLimit> {
        Default::default()
    }
}

impl Ranking for StandIn {
    fn ranking_snapshot(&self) -> BoxFuture<'_, Result<Snapshot>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: ranking_snapshot")) })
    }
}

impl crate::daemon::Handovers for StandIn {
    fn hands_over(&self) -> BoxFuture<'_, bool> {
        let hands_over = self.hands_over.load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { hands_over })
    }

    fn hand_over(
        &self,
        _: tokio::time::Instant,
        handing: shoal_protocol::hub::packages::Handing,
    ) -> BoxFuture<'_, Vec<(Space, shoal_protocol::hub::packages::Handed)>> {
        self.handings.lock().unwrap().push(handing);
        let handed = std::mem::take(&mut *self.handed.lock().unwrap());
        Box::pin(async move { handed })
    }

    fn stay(&self) -> BoxFuture<'_, ()> {
        (self.stays).fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {})
    }
}

/// What [`space_facts`] reads of a space, before its condition.
const SPACE_FACTS: &str = "SELECT s.id,s.kind,s.repository_id,s.name,w.state,
    COALESCE(w.setup_finished,0),
    EXISTS (SELECT 1 FROM space_completion c WHERE c.space_id=s.id)
    FROM spaces s LEFT JOIN workspaces w USING(id)";

fn space_facts(row: &rusqlite::Row<'_>) -> rusqlite::Result<SpaceFacts> {
    Ok(SpaceFacts {
        id: row.get(0)?,
        kind: row.get(1)?,
        repository: row.get(2)?,
        name: row.get(3)?,
        state: row.get(4)?,
        setup_finished: row.get(5)?,
        done: row.get(6)?,
    })
}

/// No removals: the stand-in removes no space.
pub(crate) struct StandInRemovals;

impl Removals for StandInRemovals {
    fn require_taking_work(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        shoal_store::require_ready(db, id)
    }

    fn being_removed(&self, _: &SpaceId) -> bool {
        false
    }
}

/// Records without the other subsystems' tables: no commands an agent
/// left, no restores Shoal queued, and the spaces a test makes watch an
/// open PR.
#[derive(Default)]
pub(crate) struct StandInRecords {
    pub(crate) open_prs: Mutex<HashSet<SpaceId>>,
}

impl Records for StandInRecords {
    fn end_commands(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn stop_recorded(&self, db: &Connection, id: &ExecutionId) -> Result<bool> {
        Ok(db.query_row(
            "SELECT EXISTS (SELECT 1 FROM agent_stops WHERE execution_id=?1)",
            [id],
            |row| row.get(0),
        )?)
    }

    fn wake_asleep(&self, _: &Connection, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn receives_open_pr_activity(
        &self,
        _: &Connection,
        id: &SpaceId,
        _: &[SpaceId],
    ) -> Result<bool> {
        Ok(self.open_prs.lock().unwrap().contains(id))
    }

    fn space_done(&self, db: &Connection, id: &SpaceId) -> Result<bool> {
        shoal_store::exists(db, "SELECT 1 FROM space_completion WHERE space_id=?1", [id])
    }

    fn space_is_workspace(&self, db: &Connection, id: &SpaceId) -> Result<bool> {
        shoal_store::exists(
            db,
            "SELECT 1 FROM spaces WHERE id=?1 AND kind='workspace'",
            [id],
        )
    }

    fn ensure_takes_alerts(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        let present = shoal_store::exists(db, "SELECT 1 FROM spaces WHERE id=?1", [id])?;
        anyhow::ensure!(present, "no space {id}");
        Ok(())
    }

    fn repository_registered(&self, db: &Connection, id: &RepositoryId) -> Result<bool> {
        shoal_store::exists(db, "SELECT 1 FROM repositories WHERE id=?1", [id])
    }

    fn space_is_chat(&self, db: &Connection, id: &SpaceId) -> Result<bool> {
        shoal_store::exists(db, "SELECT 1 FROM spaces WHERE id=?1 AND kind='chat'", [id])
    }

    fn selected_space(
        &self,
        db: &Connection,
        selector: &SpaceSelector,
    ) -> Result<Option<(SpaceKind, SpaceId)>> {
        let Some(id) = selector.given_id() else {
            return Ok(None);
        };
        Ok(db
            .query_row("SELECT kind FROM spaces WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?
            .map(|kind| (kind, id.to_owned())))
    }

    fn parent_ids(&self, db: &Connection) -> Result<HashMap<SpaceId, SpaceId>> {
        Ok(db
            .prepare("SELECT space_id, parent_id FROM space_parents")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn agent_token(&self, _: &Connection, _: &SpaceId) -> Result<Option<ScopeToken>> {
        Ok(None)
    }

    fn preparation_waits(&self, _: &Connection, _: &SpaceId) -> Result<Option<PreparationWait>> {
        Ok(None)
    }

    fn note_preparation_stopped(&self, _: &Connection, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn agent_ended(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn release_decided_by(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<Vec<ApprovalRequest>> {
        Ok(Vec::new())
    }

    fn record_demo(
        &self,
        _: &Transaction<'_>,
        _: &ExecutionId,
        _: &SpaceId,
        _: &Launch,
    ) -> Result<()> {
        Ok(())
    }

    fn mark_demo_run(&self, _: &Transaction<'_>, _: &ExecutionId) -> Result<()> {
        Ok(())
    }

    fn demo_launch(&self, _: &Connection, _: &ExecutionId) -> Result<Option<Launch>> {
        Ok(None)
    }

    fn spaces(&self, db: &Connection) -> Result<Vec<SpaceFacts>> {
        Ok(db
            .prepare(&format!("{SPACE_FACTS} ORDER BY s.rowid"))?
            .query_map([], space_facts)?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn space_facts(&self, db: &Connection, id: &SpaceId) -> Result<Option<SpaceFacts>> {
        Ok(db
            .query_row(&format!("{SPACE_FACTS} WHERE s.id=?1"), [id], space_facts)
            .optional()?)
    }

    fn workspace(&self, db: &Connection, id: &SpaceId) -> Result<Option<Workspace>> {
        Ok(db
            .query_row(
                &format!(
                    "SELECT {} FROM spaces JOIN workspaces USING(id) WHERE id=?1",
                    shoal_store::WORKSPACE_COLUMNS
                ),
                [id],
                shoal_store::workspace,
            )
            .optional()?)
    }

    fn issue_workspaces(
        &self,
        _: &Connection,
        _: &RepositoryId,
        _: &str,
    ) -> Result<Vec<Workspace>> {
        Ok(Vec::new())
    }

    fn converting_to_workspaces(&self, _: &Connection) -> Result<HashSet<SpaceId>> {
        Ok(HashSet::new())
    }

    fn recorded_job_workspaces(&self, _: &Connection) -> Result<HashSet<SpaceId>> {
        Ok(HashSet::new())
    }

    fn runners_registered(&self, _: &Connection) -> Result<bool> {
        Ok(false)
    }

    fn record_space_mode(
        &self,
        _: &Connection,
        _: &SpaceId,
        _: &ModeName,
        _: &PermissionsConfig,
    ) -> Result<()> {
        Ok(())
    }

    fn reserve_setup(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<bool> {
        Ok(true)
    }

    fn end_setup(&self, _: &Transaction<'_>, _: &SpaceId, _: Option<&str>) -> Result<()> {
        Ok(())
    }

    fn space_status(
        &self,
        db: &Connection,
        id: &SpaceId,
        _: &dyn Fn(&str) -> bool,
    ) -> Result<Option<SpaceStatus>> {
        let name: Option<String> = db
            .query_row("SELECT name FROM spaces WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(name.map(|name| SpaceStatus {
            name,
            thread: None,
            approval: None,
            done: None,
            ready: None,
            progress: Steps::default(),
        }))
    }
}
