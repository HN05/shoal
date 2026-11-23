//! A stand-in for the rest of the daemon, which coordination's tests build
//! the service over: a temporary store with the spaces a test records and
//! the repositories it registers, each with a local config a test may set,
//! the agents a test says run, and the alerts queued for them as the
//! agents' queue records them. Nothing runs on another machine, no file is
//! attached and no review or proposal is pending; a test that needs one of
//! those drives a whole daemon instead.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context as _, Result, bail};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_daemon_base::{
    alerts::Sent,
    config_state::ConfigState,
    gates::Gates,
    notifications::{Coordinators, Notifications, Recorded, Source},
    signals::Signals,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::approvals::{ApprovalRequest, ProposalDecision};
use shoal_protocol::config::permissions::Permissions;
use shoal_protocol::files::{Attacher, FileSource, NewFile, SentFile};
use shoal_protocol::hub::Member;
use shoal_protocol::hub::directory::HubView;
use shoal_protocol::model::{Located, QueueEntry, Repository, Space, SpaceTarget, Workspace};
use shoal_protocol::paths::Paths;
use shoal_protocol::protocol::{Body, Operation};
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::threads::{Outcome, Thread};
use shoal_store::Store;
use shoal_store::tree::TrackedAgents;

use crate::{
    Coordination,
    alerts::NewAlert,
    config::{
        Config, Effective,
        repo::{ConfigLayers, RepoConfig},
    },
    daemon::{
        Agents, Attaching, Callers, Files, Hub, KeptFinding, Links, PassedUp, Records, Reported,
        Repositories, Reviews, Spaces,
    },
    repository::Selector,
    scope::Caller,
};

/// The service over a stand-in daemon.
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) coordination: Arc<Coordination>,
    pub(crate) daemon: Arc<StandIn>,
    pub(crate) notifications: Arc<Notifications>,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        Self::with_config("").await
    }

    /// The service under the global config `text`.
    pub(crate) async fn with_config(text: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(std::fs::canonicalize(root.path()).unwrap());
        std::fs::create_dir_all(&paths.state).unwrap();
        let store = shoal_store::test_support::unsynced_store(paths.state.join("state.db"))
            .await
            .unwrap();
        let notifications = Arc::new(Notifications::new(store.clone()));
        let daemon = Arc::new(StandIn {
            store: store.clone(),
            config: Arc::new(ConfigState::new(shoal_core::test_support::global_config(
                text,
            ))),
            coordination: OnceLock::new(),
            spaces: Mutex::default(),
            agents: Mutex::default(),
            local: Mutex::default(),
        });
        let coordination = Arc::new(Coordination::new(
            store,
            Arc::new(Gates::default()),
            daemon.config.clone(),
            notifications.clone(),
            Arc::new(Signals::default()),
            daemon.clone(),
        ));
        let _ = daemon.coordination.set(coordination.clone());
        Self {
            root,
            coordination,
            daemon,
            notifications,
        }
    }

    /// Record a ready workspace `name` of the fixture's repository, below
    /// `parent` when it names one.
    pub(crate) async fn workspace(&self, name: &str, parent: Option<&SpaceId>) -> Workspace {
        self.workspace_in("repo", name, parent, None).await
    }

    /// Register the repository `id`.
    pub(crate) async fn repository(&self, id: &str) -> RepositoryId {
        let id = RepositoryId::from(id);
        let registered = id.clone();
        (self.daemon.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO repositories(id,path,source,last_used)
                     VALUES (?1,'/'||?1,'/'||?1,1)",
                    [&registered],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        id
    }

    /// Give the repository `id` the local config `toml`, which the
    /// settings of its spaces take over the global config.
    pub(crate) fn set_local_config(&self, id: &RepositoryId, toml: &str) {
        let config = toml::from_str(toml).unwrap();
        self.daemon.local.lock().unwrap().insert(id.clone(), config);
    }

    /// Record a ready workspace `name` of `repository` in `mode`, below
    /// `parent` when it names one.
    pub(crate) async fn workspace_in(
        &self,
        repository: &str,
        name: &str,
        parent: Option<&SpaceId>,
        mode: Option<&str>,
    ) -> Workspace {
        let repository = self.repository(repository).await;
        let path = self.root.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let mut workspace =
            shoal_protocol::test_support::workspace(name, repository.as_str(), name, &path);
        let number = self.daemon.next_number();
        workspace.number = number;
        workspace.mode = mode.map(|mode| mode.parse().unwrap());
        let (id, parent) = (workspace.id.clone(), parent.cloned());
        let mode = mode.map(str::to_owned);
        (self.daemon.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO spaces(id,kind,repository_id,name,number,mode)
                     VALUES (?1,'workspace',?2,?1,?3,?4)",
                    rusqlite::params![id, repository, number, mode],
                )?;
                tx.execute(
                    "INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
                     VALUES (?1,'/work/'||?1,?1,'ready','abc','/admin')",
                    [&id],
                )?;
                if let Some(parent) = parent {
                    tx.execute(
                        "INSERT INTO space_parents(space_id,parent_id) VALUES (?1,?2)",
                        [&id, &parent],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let space = shoal_protocol::test_support::space(&workspace);
        self.daemon.spaces.lock().unwrap().push(space);
        workspace
    }

    /// Publish `text` as the global config.
    pub(crate) fn configure(&self, text: &str) {
        let config = shoal_core::test_support::global_config(text);
        self.daemon.config.publish(config);
    }

    /// The store the service and the stand-in share.
    pub(crate) fn store(&self) -> &Store {
        &self.daemon.store
    }

    /// Record a chat `name` in `mode`, of `repository` when it names one,
    /// below `parent` when it names one.
    pub(crate) async fn chat(
        &self,
        name: &str,
        mode: &str,
        repository: Option<&RepositoryId>,
        parent: Option<&SpaceId>,
    ) -> Space {
        let number = self.daemon.next_number();
        let path = self.root.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let chat: Space = serde_json::from_value(serde_json::json!({
            "id": name,
            "number": number,
            "name": name,
            "repository_id": repository,
            "mode": mode,
            "workdir": {"kind": "checkout", "path": path},
            "kind": "chat",
            "started_at": 0,
        }))
        .unwrap();
        let (id, repository, parent) = (chat.id.clone(), repository.cloned(), parent.cloned());
        let mode = mode.to_owned();
        (self.daemon.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO spaces(id,kind,repository_id,name,number,mode)
                     VALUES (?1,'chat',?2,?1,?3,?4)",
                    rusqlite::params![id, repository, number, mode],
                )?;
                tx.execute("INSERT INTO chats(id) VALUES (?1)", [&id])?;
                if let Some(parent) = parent {
                    tx.execute(
                        "INSERT INTO space_parents(space_id,parent_id) VALUES (?1,?2)",
                        [&id, &parent],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        self.daemon.spaces.lock().unwrap().push(chat.clone());
        chat
    }

    /// Record `assignments` as the `--permission` given to the agent of the
    /// space `id`.
    pub(crate) fn give(&self, id: &SpaceId, assignments: &[&str]) {
        let assignments: Vec<String> = assignments.iter().map(|a| (*a).to_owned()).collect();
        let narrowing = crate::config::permissions::PermissionsConfig::assigned(&assignments);
        let mut spaces = self.daemon.spaces.lock().unwrap();
        let space = spaces.iter_mut().find(|space| space.id == *id).unwrap();
        space.narrowing = narrowing.unwrap();
    }

    /// The space `id`, as the daemon reads it.
    pub(crate) fn space_of(&self, id: &SpaceId) -> Space {
        self.daemon.find(id).unwrap()
    }

    /// Say that the agent of the space `id` runs.
    pub(crate) fn run_agent(&self, id: &SpaceId) {
        self.daemon.agents.lock().unwrap().insert(id.clone());
    }

    /// The messages of the unread alerts queued for the agents of the
    /// space `id`, oldest first.
    pub(crate) async fn queued(&self, id: &SpaceId) -> Vec<String> {
        let id = id.clone();
        (self.daemon.store)
            .read(move |db| {
                Ok(db
                    .prepare(
                        "SELECT message FROM alerts WHERE recipient_id=?1 AND read=0 ORDER BY id",
                    )?
                    .query_map([&id], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
            .unwrap()
    }
}

impl std::ops::Deref for Fixture {
    type Target = StandIn;

    /// The stand-in daemon, which the service's operations take.
    fn deref(&self) -> &StandIn {
        &self.daemon
    }
}

/// The rest of the daemon, as the fixture records it.
pub(crate) struct StandIn {
    store: Store,
    /// The global config, which the service shares.
    config: Arc<ConfigState>,
    /// The service, which routes the alerts the base's notifications pass
    /// to a coordinator.
    coordination: OnceLock<Arc<Coordination>>,
    spaces: Mutex<Vec<Space>>,
    /// The spaces whose agent runs.
    agents: Mutex<HashSet<SpaceId>>,
    /// Each repository's local config.
    local: Mutex<HashMap<RepositoryId, RepoConfig>>,
}

impl StandIn {
    /// The layers over the global config of `repository`.
    fn layers(&self, repository: Option<&RepositoryId>) -> ConfigLayers {
        let local = self.local.lock().unwrap();
        ConfigLayers {
            local_config: (repository.and_then(|id| local.get(id)))
                .cloned()
                .unwrap_or_default(),
            ..ConfigLayers::default()
        }
    }
}

impl StandIn {
    fn next_number(&self) -> SpaceNumber {
        let count = self.spaces.lock().unwrap().len();
        SpaceNumber::from(count as u32 + 1)
    }

    fn find(&self, id: &SpaceId) -> Option<Space> {
        let spaces = self.spaces.lock().unwrap();
        spaces.iter().find(|space| space.id == *id).cloned()
    }

    fn selected(&self, selector: &SpaceSelector) -> Result<Space> {
        let spaces = self.spaces.lock().unwrap();
        let found = spaces.iter().find(|space| match selector {
            SpaceSelector::Id(id) => space.id == *id,
            SpaceSelector::Number(number) => space.number == *number,
            _ => false,
        });
        found
            .cloned()
            .with_context(|| format!("{NOT_HERE}: {selector}"))
    }

    /// Queue `alert` for the agents of the space `id`, as the agents' queue
    /// records it.
    async fn queue(&self, id: &SpaceId, alert: NewAlert) -> Result<i64> {
        let id = id.clone();
        (self.store)
            .write(move |tx| shoal_daemon_base::alerts::insert(tx, Some(&id), &alert, true, false))
            .await
    }
}

/// What a test reached that the stand-in does not offer.
const NOT_HERE: &str = "coordination's stand-in does not offer";

impl Coordinators for StandIn {
    fn tell<'a>(
        &'a self,
        alert: NewAlert,
        for_user: bool,
        source: Source,
    ) -> BoxFuture<'a, Option<Sent>> {
        let coordination = self.coordination.get().cloned();
        Box::pin(async move {
            let coordination = coordination?;
            coordination
                .tell_coordinator(self, alert, for_user, source)
                .await
        })
    }
}

impl Spaces for StandIn {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        let space = self.selected(selector);
        Box::pin(async { space })
    }

    fn space_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>> {
        let space = self.find(id).with_context(|| format!("{NOT_HERE}: {id}"));
        Box::pin(async { space })
    }

    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        let space = self.find(id);
        Box::pin(async { Ok(space) })
    }

    fn space_anywhere<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Located>> {
        let space = self.selected(selector);
        Box::pin(async { Ok(Located::Here(Box::new(space?))) })
    }

    fn space_id_anywhere<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<SpaceId>> {
        let space = self.selected(selector);
        Box::pin(async { Ok(space?.id) })
    }

    fn spaces_within<'a>(&'a self, root: Option<&'a SpaceId>) -> BoxFuture<'a, Result<Vec<Space>>> {
        Box::pin(async move {
            let spaces = self.spaces.lock().unwrap().clone();
            let Some(root) = root else {
                return Ok(spaces);
            };
            let root = root.clone();
            let below = (self.store)
                .read(move |db| shoal_store::tree::subtree(db, &root))
                .await?;
            Ok(spaces
                .into_iter()
                .filter(|space| below.contains(&space.id))
                .collect())
        })
    }

    fn find_spaces<'a>(
        &'a self,
        _: &'a SpaceTarget,
        _: Vec<Space>,
    ) -> BoxFuture<'a, Result<Vec<Space>>> {
        Box::pin(async { bail!("{NOT_HERE}: finding spaces by item or resource") })
    }

    fn being_removed(&self, _: &Space) -> bool {
        false
    }

    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        let workspace = self.selected(selector).and_then(|space| {
            (space.into_workspace()).map_err(|_| anyhow::anyhow!("{NOT_HERE}: a chat"))
        });
        Box::pin(async { workspace })
    }

    fn find_workspace<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Workspace>>> {
        let workspace = self.find(id).and_then(|space| space.into_workspace().ok());
        Box::pin(async { Ok(workspace) })
    }

    fn list_workspaces(&self) -> BoxFuture<'_, Result<Vec<Workspace>>> {
        let spaces = self.spaces.lock().unwrap().clone();
        let workspaces = spaces
            .into_iter()
            .filter_map(|space| space.into_workspace().ok())
            .collect();
        Box::pin(async { Ok(workspaces) })
    }

    fn chat_home<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(async { Ok(None) })
    }

    fn user_holds<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn hold_decision_guard<'a>(
        &'a self,
        _: &'a ApprovalRequest,
        _: bool,
    ) -> BoxFuture<'a, Result<Option<Box<dyn Send>>>> {
        Box::pin(async { Ok(None) })
    }

    fn ensure_merge_current<'a>(
        &'a self,
        _: &'a ApprovalRequest,
        _: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl Files for StandIn {
    fn attacher<'a>(&'a self, _: Option<&'a Caller>) -> BoxFuture<'a, Attacher> {
        Box::pin(async { Attacher::User })
    }

    fn attach_files<'a>(
        &'a self,
        _: &'a Space,
        _: FileSource,
        _: &'a str,
        _: Vec<NewFile>,
        _: &'a Attacher,
    ) -> BoxFuture<'a, Result<Attaching>> {
        Box::pin(async { bail!("{NOT_HERE}: attached files") })
    }

    fn sent_files(&self, paths: Vec<PathBuf>) -> BoxFuture<'_, Result<Vec<SentFile>>> {
        Box::pin(async move {
            match paths.is_empty() {
                true => Ok(Vec::new()),
                false => bail!("{NOT_HERE}: attached files"),
            }
        })
    }

    fn queue_with_files<'a>(
        &'a self,
        _: Space,
        _: NewAlert,
        _: Vec<NewFile>,
        _: &'a Attacher,
    ) -> BoxFuture<'a, Result<i64>> {
        Box::pin(async { bail!("{NOT_HERE}: attached files") })
    }
}

impl Agents for StandIn {
    fn tracked_agents(&self) -> BoxFuture<'_, TrackedAgents> {
        let agents: Vec<_> = self.agents.lock().unwrap().iter().cloned().collect();
        Box::pin(async move { TrackedAgents::of(&agents) })
    }

    fn running_agent_workspaces(&self) -> BoxFuture<'_, Vec<SpaceId>> {
        let agents = self.agents.lock().unwrap().iter().cloned().collect();
        Box::pin(async { agents })
    }

    fn recorded_agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        let agents = self.agents.lock().unwrap().clone();
        Box::pin(async { Ok(agents) })
    }

    fn queued_agent_workspaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        Box::pin(async { Ok(HashSet::new()) })
    }

    fn has_saved_agent(&self, _: &SpaceId) -> bool {
        false
    }

    fn queue_alert<'a>(
        &'a self,
        id: &'a SpaceId,
        alert: NewAlert,
        _: bool,
    ) -> BoxFuture<'a, Result<i64>> {
        Box::pin(self.queue(id, alert))
    }

    fn queue_agent_message<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>> {
        let about = shoal_daemon_base::alerts::About::default();
        let alert = NewAlert::shoal(kind, about, message);
        Box::pin(async move { self.queue(id, alert).await.map(drop) })
    }

    fn mark_messages_delivered<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: Vec<i64>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn queue_entries(&self) -> BoxFuture<'_, Result<Vec<QueueEntry>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn launch_parents(&self, _: &[QueueEntry]) -> HashMap<QueueEntryId, SpaceId> {
        HashMap::new()
    }

    fn started_launch<'a>(&'a self, _: &'a QueueEntryId) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(async { Ok(None) })
    }
}

impl Callers for StandIn {
    fn execution_caller<'a>(&'a self, _: &'a ScopeToken) -> BoxFuture<'a, Option<Caller>> {
        Box::pin(async { None })
    }

    fn operation_caller(&self, _: &ScopeToken) -> Option<Caller> {
        None
    }
}

impl Hub for StandIn {
    fn names_vouched_own(&self, _: &Caller, _: &SpaceSelector) -> bool {
        false
    }

    fn vouched_caller(&self, _: &ScopeToken) -> Option<Option<Caller>> {
        None
    }

    fn runs_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn ancestors_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Vec<SpaceId>> {
        Box::pin(async { Vec::new() })
    }

    fn reached_through_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Vec<SpaceId>> {
        Box::pin(async { Vec::new() })
    }

    fn reaches_elsewhere<'a>(
        &'a self,
        _: &'a Caller,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn deciders_elsewhere<'a>(&'a self, _: &'a SpaceId, _: bool) -> BoxFuture<'a, Vec<SpaceId>> {
        Box::pin(async { Vec::new() })
    }

    fn permissions_elsewhere<'a>(
        &'a self,
        _: &'a SpaceId,
        _: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Option<Permissions>>> {
        Box::pin(async { Ok(None) })
    }

    fn reported_above<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Reported>>> {
        Box::pin(async { Ok(None) })
    }

    fn repositories_view(&self) -> BoxFuture<'_, Result<Option<HubView>>> {
        Box::pin(async { Ok(None) })
    }

    fn acting_space<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<(SpaceId, String)>> {
        let acting = match caller {
            Some(caller) => self.find(&caller.space_id),
            None => self.selected(selector).ok(),
        };
        let acting = acting
            .map(|space| (space.id, space.name.to_string()))
            .context(NOT_HERE);
        Box::pin(async { acting })
    }

    fn message_elsewhere<'a>(
        &'a self,
        _: &'a SpaceId,
        _: AlertKind,
        _: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { bail!("{NOT_HERE}: another machine") })
    }

    fn ask_machine_or_hold<'a>(
        &'a self,
        _: &'a Member,
        _: Operation,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Body>> {
        Box::pin(async { bail!("{NOT_HERE}: another machine") })
    }

    fn pass_up<'a>(&'a self, _: SpaceId, _: PassedUp<'a>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { bail!("{NOT_HERE}: another machine") })
    }
}

impl Repositories for StandIn {
    fn repository(&self, _: Selector) -> BoxFuture<'_, Result<Repository>> {
        Box::pin(async { bail!("{NOT_HERE}: a registered repository") })
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn space_settings_in<'a>(
        &'a self,
        id: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.config.current().resolve(&self.layers(id));
        Box::pin(async { settings })
    }

    fn space_settings_from<'a>(
        &'a self,
        config: &'a Config,
        space: &'a Space,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>> {
        // A space's own repository, unless the action is in another.
        let repository = repository.or(space.repository_id.as_ref());
        let settings = config.resolve(&self.layers(repository));
        Box::pin(async { settings })
    }
}

impl Reviews for StandIn {
    fn answer_posts(&self) {}

    fn give_gate<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn keep_false_positive(
        &self,
        _: Thread,
        _: Option<Outcome>,
    ) -> BoxFuture<'_, Result<Option<Box<dyn KeptFinding + '_>>>> {
        Box::pin(async { Ok(None) })
    }
}

impl Links for StandIn {
    fn ensure_not_pushing(&self, _: &ApprovalRequest) -> Result<()> {
        Ok(())
    }

    fn decide_proposal<'a>(
        &'a self,
        _: ApprovalRequest,
        _: bool,
        _: ProposalDecision,
        _: Option<BTreeMap<String, String>>,
        _: Option<&'a SpaceId>,
    ) -> BoxFuture<'a, Result<ApprovalRequest>> {
        Box::pin(async { bail!("{NOT_HERE}: proposals") })
    }

    fn prune_proposal_files<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Records for StandIn {
    fn require_taking_work(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        shoal_store::require_ready_space(db, id)
    }

    fn place_approved_hold(&self, _: &Transaction<'_>, _: &ApprovalRequest) -> Result<()> {
        Ok(())
    }

    fn agent_turn(
        &self,
        _: &Connection,
        _: &SpaceId,
    ) -> Result<(Option<i64>, Option<ExecutionId>)> {
        Ok((None, None))
    }

    fn queue_alert_in(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        alert: &NewAlert,
    ) -> Result<Recorded> {
        let id = shoal_daemon_base::alerts::insert(tx, Some(id), alert, true, false)?;
        Ok(Recorded { id, new: true })
    }
}
