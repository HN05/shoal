//! A stand-in for the rest of the daemon, which the tests of leases build
//! the service over, with one ready workspace.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_daemon_base::approval_requests::ReasonRequired;
use shoal_daemon_base::{
    alerts::{NewAlert, Sent},
    config_state::ConfigState,
    gates::Gates,
    notifications::{Coordinators, Notifications, Source},
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::acquirers::Acquirer;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::approvals::{ApprovalRequest, DecisionStatus, Target};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_store::test_support::seeds;

use crate::{
    Leases,
    callers::Caller,
    config::{Config, Effective, repo::ConfigLayers},
    daemon::{Agents, Approvals, Records, Settings, Sleeper, Spaces, Unborrowable},
    hooks::{Hook, HookKind},
    model::{Repository, Workspace},
};

/// The service over a stand-in daemon with one ready workspace.
pub(crate) struct Fixture {
    _root: tempfile::TempDir,
    pub(crate) leases: Leases,
    pub(crate) daemon: StandIn,
    pub(crate) records: Arc<StandInRecords>,
    pub(crate) workspace: Workspace,
}

impl Fixture {
    /// A workspace whose repository's worktree file is `repo` under the
    /// global config `global`.
    pub(crate) async fn new(global: &str, repo: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = shoal_store::test_support::unsynced_store(root.path().join("state.db"))
            .await
            .unwrap();
        let workspace = workspace(root.path().join("worktree"));
        record(&store, &workspace).await;
        let config = Arc::new(ConfigState::new(shoal_core::test_support::global_config(
            global,
        )));
        let notifications = Arc::new(Notifications::new(store.clone()));
        let records = Arc::new(StandInRecords::default());
        let gates = Arc::new(Gates::default());
        let leases = Leases::new(store, gates, config, notifications, records.clone());
        let daemon = StandIn {
            workspace: workspace.clone(),
            layers: Mutex::new(layers(repo)),
            hooks: Mutex::default(),
            hooks_run: Mutex::default(),
            agents: Mutex::default(),
            sleepers: Mutex::default(),
            told: Mutex::default(),
            touched: Mutex::default(),
            announced: Mutex::default(),
        };
        Self {
            _root: root,
            leases,
            daemon,
            records,
            workspace,
        }
    }

    /// Make `repo` the worktree file of the workspace's repository.
    pub(crate) fn configure(&self, repo: &str) {
        *self.daemon.layers.lock().unwrap() = layers(repo);
    }

    pub(crate) fn selector(&self) -> SpaceSelector {
        SpaceSelector::id(&self.workspace.id)
    }

    /// Record a leased simulator named `name` of the workspace that
    /// `acquirer` acquired, without a device, so releasing it runs no
    /// simctl.
    pub(crate) async fn lease_simulator(&self, name: &str, acquirer: Acquirer) {
        let leased = shoal_protocol::sim::SimulatorState::Leased;
        self.record_simulator(name, acquirer, leased).await;
    }

    /// Record a simulator named `name` of the workspace in `state` that
    /// `acquirer` claimed, without a device.
    pub(crate) async fn record_simulator(
        &self,
        name: &str,
        acquirer: Acquirer,
        state: shoal_protocol::sim::SimulatorState,
    ) {
        let sim = shoal_protocol::sim::Simulator {
            id: name.into(),
            udid: None,
            device: "phone".into(),
            runtime: "ios".into(),
            workspace_id: Some(self.workspace.id.clone()),
            last_workspace_id: None,
            lease_name: Some(name.into()),
            reason: None,
            state,
            last_used: crate::time::UnixTime::now(),
            error: None,
            installed_apps: None,
            acquirer,
        };
        self.leases.record_simulator(&sim).await.unwrap();
    }

    /// The names of the workspace's simulator leases.
    pub(crate) async fn simulators(&self) -> Vec<String> {
        let sims = self.leases.list_simulators(Some(&self.workspace.id));
        let mut names: Vec<_> = (sims.await.unwrap().into_iter())
            .filter(|sim| sim.workspace_id.is_some())
            .filter_map(|sim| sim.lease_name)
            .collect();
        names.sort();
        names
    }

    /// The notifications recorded for the user, as kind and message.
    pub(crate) async fn notified(&self) -> Vec<(AlertKind, String)> {
        let listed = self.leases.notifications.list(false, 100).await.unwrap();
        (listed.into_iter())
            .map(|notification| (notification.kind, notification.message))
            .collect()
    }
}

/// The layers of a repository whose worktree file is `repo`.
fn layers(repo: &str) -> ConfigLayers {
    ConfigLayers {
        worktree_file: crate::config::repo::parse(repo).unwrap(),
        ..ConfigLayers::default()
    }
}

/// A ready workspace with its worktree at `path`.
fn workspace(path: PathBuf) -> Workspace {
    serde_json::from_value(serde_json::json!({
        "id": "workspace",
        "number": 1,
        "name": "worker",
        "repository_id": "repo",
        "workdir": {
            "kind": "worktree",
            "workspace": {"id": "workspace", "name": "worker", "branch": "worker"},
            "path": path,
        },
        "kind": "workspace",
        "path": path,
        "branch": "worker",
        "state": "ready",
        "checkout": {"state": "created", "base": {"commit": "abc"}, "git_dir": "/admin"},
        "error": null,
        "base_commit": null,
        "base_ref": null,
        "git_dir": null,
    }))
    .unwrap()
}

/// Record `workspace` and its repository in `store`.
async fn record(store: &shoal_store::Store, workspace: &Workspace) {
    let recorded = workspace.clone();
    store
        .write(move |tx| {
            let repository = shoal_protocol::test_support::repository("repo", "/repo".as_ref());
            seeds::repository(tx, &repository)?;
            seeds::workspace(tx, &recorded)?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The rest of the daemon as the fixture's workspace sees it.
pub(crate) struct StandIn {
    workspace: Workspace,
    layers: Mutex<ConfigLayers>,
    /// The hooks configured by kind.
    pub(crate) hooks: Mutex<HashMap<HookKind, StandInHook>>,
    /// The executables of the hooks run, in order.
    pub(crate) hooks_run: Mutex<Vec<PathBuf>>,
    /// The spaces whose agent has not ended.
    pub(crate) agents: Mutex<HashSet<SpaceId>>,
    /// The asleep agents, lowest-ranked first.
    pub(crate) sleepers: Mutex<Vec<Sleeper>>,
    /// What each space's agent was told, in order.
    pub(crate) told: Mutex<Vec<(SpaceId, String)>>,
    /// The workspaces whose requests counted as their activity.
    pub(crate) touched: Mutex<Vec<SpaceId>>,
    /// The approval requests announced to whoever decides them.
    pub(crate) announced: Mutex<Vec<ApprovalRequest>>,
}

impl Spaces for StandIn {
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(async move {
            let selected = *selector == SpaceSelector::id(&self.workspace.id);
            selected
                .then(|| self.workspace.clone())
                .with_context(|| format!("no workspace {selector}"))
        })
    }

    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(async move { self.workspace(&SpaceSelector::id(id)).await })
    }

    fn touch<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        self.touched.lock().unwrap().push(id.clone());
        Box::pin(async {})
    }

    fn verify_worktree<'a>(&'a self, _workspace: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn fail_setup<'a>(&'a self, _id: &'a SpaceId, message: String) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { anyhow::bail!("setup failed: {message}") })
    }
}

impl Settings for StandIn {
    fn settings<'a>(
        &'a self,
        config: &'a Config,
        _workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        let layers = self.layers.lock().unwrap().clone();
        Box::pin(async move { config.resolve(&layers) })
    }

    fn space_settings<'a>(
        &'a self,
        config: &'a Config,
        _selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Effective>> {
        let layers = self.layers.lock().unwrap().clone();
        Box::pin(async move { config.resolve(&layers) })
    }

    fn hook<'a>(
        &'a self,
        _workspace: &'a Workspace,
        kind: HookKind,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>> {
        let hooks = self.hooks.lock().unwrap();
        let executable = hooks.get(&kind).map(|hook| hook.executable.clone());
        Box::pin(async move { Ok(executable) })
    }

    fn run_hook<'a>(
        &'a self,
        _workspace: &'a Workspace,
        _hook: Hook<'a>,
        executable: &'a Path,
    ) -> BoxFuture<'a, Result<()>> {
        let hooks = self.hooks.lock().unwrap();
        let hook = hooks.values().find(|hook| hook.executable == executable);
        let fails = hook.expect("a configured hook").fails;
        self.hooks_run.lock().unwrap().push(executable.to_owned());
        Box::pin(async move {
            ensure!(!fails, "the hook failed");
            Ok(())
        })
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

/// Every scoped caller is its space's agent; nothing runs a demo.
impl Agents for StandIn {
    fn caller_agent<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Option<SpaceId>> {
        Box::pin(async move { Some(caller.space_id.clone()) })
    }

    fn runs_demo<'a>(&'a self, _execution: &'a ExecutionId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn recorded_executions(&self) -> BoxFuture<'_, Result<HashSet<ExecutionId>>> {
        Box::pin(async { Ok(HashSet::new()) })
    }

    fn agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        let agents = self.agents.lock().unwrap().clone();
        Box::pin(async move { Ok(agents) })
    }

    fn has_queued_launch<'a>(&'a self, _id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// Every asleep agent yields to every request.
    fn sleepers<'a>(&'a self, _: Option<&'a SpaceId>) -> BoxFuture<'a, Result<Vec<Sleeper>>> {
        let sleepers = self.sleepers.lock().unwrap().clone();
        Box::pin(async move { Ok(sleepers) })
    }

    fn tell_agent<'a>(&'a self, id: &'a SpaceId, message: String) -> BoxFuture<'a, ()> {
        self.told.lock().unwrap().push((id.clone(), message));
        Box::pin(async {})
    }
}

impl Approvals for StandIn {
    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        self.announced.lock().unwrap().push(request.clone());
        Box::pin(async {})
    }
}

impl Coordinators for StandIn {
    fn tell<'a>(
        &'a self,
        _alert: NewAlert,
        _for_user: bool,
        _source: Source,
    ) -> BoxFuture<'a, Option<Sent>> {
        Box::pin(async { None })
    }
}

/// A configured hook, and whether running it fails.
pub(crate) struct StandInHook {
    pub(crate) executable: PathBuf,
    pub(crate) fails: bool,
}

/// Approval requests by owner, target and name, each bound to its settings
/// as the daemon's are, which a test decides. Every registered repository
/// can be borrowed, and the fixture's workspace is the only space.
#[derive(Default)]
pub(crate) struct StandInRecords {
    requests: Mutex<HashMap<(SpaceId, Target, String), ApprovalRequest>>,
    /// How many requests it recorded, which numbers their keys.
    made: AtomicUsize,
}

impl StandInRecords {
    /// Decide the request for `name` of `target`.
    pub(crate) fn decide(&self, target: &Target, name: &str, status: DecisionStatus) {
        let mut requests = self.requests.lock().unwrap();
        let key = (SpaceId::from("workspace"), target.clone(), name.to_owned());
        requests.get_mut(&key).expect("a request").status = status;
    }
}

impl Records for StandInRecords {
    fn check_approval(
        &self,
        _tx: &Transaction<'_>,
        request: ApprovalRequest,
    ) -> Result<Option<ApprovalRequest>> {
        let mut requests = self.requests.lock().unwrap();
        let key = (
            request.space_id.clone(),
            request.target.clone(),
            request.name.clone(),
        );
        if !requests.contains_key(&key) {
            ensure!(!request.reason.is_empty(), ReasonRequired);
        }
        let specification = request.specification.clone();
        let stored = requests.entry(key).or_insert_with(|| ApprovalRequest {
            key: format!("request-{}", self.made.fetch_add(1, Ordering::Relaxed)),
            ..request
        });
        ensure!(
            stored.specification == specification,
            "approval request settings changed"
        );
        Ok((stored.status != DecisionStatus::Approved).then(|| stored.clone()))
    }

    fn release_approval(
        &self,
        _tx: &Transaction<'_>,
        owner: &SpaceId,
        target: &Target,
        name: &str,
    ) -> Result<bool> {
        let key = (owner.clone(), target.clone(), name.to_owned());
        Ok(self.requests.lock().unwrap().remove(&key).is_some())
    }

    fn current_approval(
        &self,
        _db: &Connection,
        owner: &SpaceId,
        target: &Target,
        name: &str,
    ) -> Result<Option<ApprovalRequest>> {
        let key = (owner.clone(), target.clone(), name.to_owned());
        Ok(self.requests.lock().unwrap().get(&key).cloned())
    }

    fn unborrowable(
        &self,
        _db: &Connection,
        _repository: &Repository,
    ) -> Result<Option<Unborrowable>> {
        Ok(None)
    }

    fn space_name(&self, _db: &Connection, id: &SpaceId) -> Result<Option<SpaceName>> {
        Ok((id.as_str() == "workspace").then(|| "worker".into()))
    }

    fn space_repository(&self, _db: &Connection, id: &SpaceId) -> Result<Option<RepositoryId>> {
        Ok((id.as_str() == "workspace").then(|| "repo".into()))
    }
}
