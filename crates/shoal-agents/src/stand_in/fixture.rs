//! The service over the stand-in, with what a test records for it.
use std::sync::Arc;

use shoal_core::{config::repo::ConfigLayers, paths::StateFiles, sessions::Host};
use shoal_daemon_base::{
    config_state::ConfigState, gates::Gates, notifications::Notifications, signals::Signals,
};
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    agent::Agent,
    model::{Repository, Space, SpacePart, Workspace},
    paths::Paths,
    scope_tokens::ScopeToken,
    state::WorkspaceState,
};

use shoal_store::test_support::seeds;

use super::{StandIn, StandInRecords, StandInRemovals};
use crate::Agents;

/// The service over a stand-in daemon.
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) agents: Arc<Agents>,
    pub(crate) daemon: Arc<StandIn>,
    pub(crate) records: Arc<StandInRecords>,
    /// The hosts the places of agents run in.
    hosts: Vec<Arc<dyn Host>>,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        Self::with_config("").await
    }

    /// The service under the global config `text` adds to the tests' base.
    pub(crate) async fn with_config(text: &str) -> Self {
        Self::build(text, Vec::new()).await
    }

    /// The service whose agents' places run in `hosts`.
    pub(crate) async fn with_session_hosts(hosts: Vec<Arc<dyn Host>>) -> Self {
        Self::build("", hosts).await
    }

    async fn build(text: &str, hosts: Vec<Arc<dyn Host>>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(std::fs::canonicalize(root.path()).unwrap());
        std::fs::create_dir_all(&paths.state).unwrap();
        let config = Arc::new(ConfigState::new(shoal_core::test_support::global_config(
            text,
        )));
        let daemon = StandIn::default();
        let records = Arc::new(StandInRecords::default());
        let agents = Arc::new(service(paths, config, records.clone(), hosts.clone()).await);
        daemon.use_service(&agents);
        Self {
            root,
            agents,
            daemon: Arc::new(daemon),
            records,
            hosts,
        }
    }

    /// The service a daemon that restarts builds over the same state,
    /// knowing only what it recorded there.
    pub(crate) async fn restart(self) -> Self {
        let paths = self.agents.paths.clone();
        let config = self.agents.config.clone();
        drop(self.agents);
        let (records, hosts) = (self.records.clone(), self.hosts.clone());
        let agents = Arc::new(service(paths, config, records, hosts).await);
        self.daemon.use_service(&agents);
        Self { agents, ..self }
    }

    /// Replace the global config with `text` after the tests' base, as a
    /// reload publishes it.
    pub(crate) fn configure(&self, text: &str) {
        (self.agents.config).publish(shoal_core::test_support::global_config(text));
    }

    /// Give the repository `id` the local config `text`, which its settings
    /// layer over the global config.
    pub(crate) fn configure_repository(&self, id: &RepositoryId, text: &str) {
        let layers = ConfigLayers {
            local_config: shoal_core::config::repo::parse(text).unwrap(),
            ..ConfigLayers::default()
        };
        let mut configured = self.daemon.repository_layers.lock().unwrap();
        configured.insert(id.clone(), layers);
    }

    /// The repository `repo`, which the fixture's workspaces belong to
    /// unless a test names another.
    pub(crate) async fn repository(&self) -> Repository {
        self.repository_named("repo").await
    }

    /// The repository `name` at the root's `name`, registered with the
    /// stand-in and recorded in the store.
    pub(crate) async fn repository_named(&self, name: &str) -> Repository {
        let path = self.root.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let repository = Repository {
            id: name.into(),
            path: path.clone(),
            source: path.display().to_string(),
            last_used: 1,
            name: None,
            workspaces_dir: path.with_extension("workspaces"),
            contribution: false,
            shared_url: None,
        };
        let recorded = repository.clone();
        (self.agents.store)
            .write(move |tx| seeds::repository(tx, &recorded))
            .await
            .unwrap();
        let mut repositories = self.daemon.repositories.lock().unwrap();
        if !repositories.iter().any(|known| known.id == repository.id) {
            repositories.push(repository.clone());
        }
        repository
    }

    /// Record a ready workspace `name`, of the fixture's repository.
    pub(crate) async fn workspace(&self, name: &str) -> Workspace {
        self.workspace_in(&self.repository().await, name).await
    }

    /// Record a ready workspace `name` of `repository`.
    pub(crate) async fn workspace_in(&self, repository: &Repository, name: &str) -> Workspace {
        self.workspace_with_id(repository, name, name).await
    }

    /// Record a ready workspace `name` of `repository` under the ID `id`,
    /// such as one a selector names, which takes a UUID.
    pub(crate) async fn workspace_with_id(
        &self,
        repository: &Repository,
        id: &str,
        name: &str,
    ) -> Workspace {
        let path = self.root.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let mut workspace =
            shoal_protocol::test_support::workspace(id, repository.id.as_str(), name, &path);
        let recorded = workspace.clone();
        workspace.number = (self.agents.store)
            .write(move |tx| seeds::workspace(tx, &recorded))
            .await
            .unwrap();
        let mut spaces = self.daemon.spaces.lock().unwrap();
        spaces.push(Space::from(workspace.clone()));
        workspace
    }

    /// The space `id` as the daemon reads it.
    pub(crate) fn space(&self, id: &SpaceId) -> Space {
        self.daemon.find(id).expect("the test recorded the space")
    }

    /// Change the space `id` as `change` does, as the daemon reads it.
    pub(crate) fn update_space(&self, id: &SpaceId, change: impl FnOnce(&mut Space)) {
        let mut spaces = self.daemon.spaces.lock().unwrap();
        change(spaces.iter_mut().find(|space| space.id == *id).unwrap());
    }

    /// Record the space `child` below `parent` in the tree.
    pub(crate) async fn place_below(&self, child: &SpaceId, parent: &SpaceId) {
        let (child, parent) = (child.clone(), parent.clone());
        (self.agents.store)
            .write(move |tx| seeds::parent(tx, &child, &parent))
            .await
            .unwrap();
    }

    /// Forget the space `id`, as once its removal ended.
    pub(crate) async fn remove(&self, id: &SpaceId) {
        let removed = id.clone();
        (self.agents.store)
            .write(move |tx| {
                tx.execute("DELETE FROM workspaces WHERE id=?1", [&removed])?;
                tx.execute("DELETE FROM spaces WHERE id=?1", [&removed])?;
                Ok(())
            })
            .await
            .unwrap();
        self.daemon
            .spaces
            .lock()
            .unwrap()
            .retain(|space| space.id != *id);
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
        self.chat_in(name, "chat", None).await
    }

    /// Record a chat `name` of `repository`, or without one, whose agent
    /// runs in `mode` in an execution of the same name.
    pub(crate) async fn chat_in(
        &self,
        name: &str,
        mode: &str,
        repository: Option<&Repository>,
    ) -> SpaceId {
        let id = self.stopped_chat(name, mode, repository).await;
        let execution = ExecutionId::from(name);
        let mut run = shoal_protocol::test_support::execution(name, id.as_str());
        run.agent = Some(Agent::from("claude"));
        run.mode = Some(mode.parse().unwrap());
        (self.agents.store)
            .write(move |tx| seeds::execution(tx, &run, None))
            .await
            .unwrap();
        (self.agents)
            .track_unconnected(&execution, &id, &name.into(), Agent::from("claude"))
            .await;
        id
    }

    /// Record a chat `name` of `repository`, or without one, in `mode`,
    /// with its scope token and no agent running.
    pub(crate) async fn stopped_chat(
        &self,
        name: &str,
        mode: &str,
        repository: Option<&Repository>,
    ) -> SpaceId {
        let path = repository.map_or_else(|| self.root.path().into(), |found| found.path.clone());
        let repository = repository.map(|repository| repository.id.clone());
        let given = repository.as_ref().map(RepositoryId::as_str);
        let mut chat = shoal_protocol::test_support::chat(name, given, mode, &path);
        let (id, recorded) = (chat.id.clone(), chat.clone());
        chat.number = (self.agents.store)
            .write(move |tx| seeds::chat(tx, &recorded))
            .await
            .unwrap();
        self.daemon.spaces.lock().unwrap().push(Space::from(chat));
        let token = ScopeToken::from(format!("{name}-token"));
        (self.records.chat_tokens.lock().unwrap()).insert(id.clone(), token);
        id
    }

    /// Run the agent of a new chat `name` of `repository` in `mode`, in an
    /// execution registered as its wrapper registers it, and its chat.
    pub(crate) async fn chat_agent(
        &self,
        name: &str,
        mode: &str,
        repository: &Repository,
    ) -> (SpaceId, crate::executions::StartedExecution) {
        let chat = self.chat_in(name, "chat", Some(repository)).await;
        let daemon = &*self.daemon;
        // Stopped, so the execution registered below is its agent.
        self.agents.end_executions(daemon, &chat).await;
        let execution = (self.agents.begin_agent_execution(daemon, chat.as_str()))
            .await
            .unwrap();
        let agent = shoal_protocol::executions::TrackedAgent {
            mode: mode.into(),
            ..shoal_protocol::executions::TrackedAgent::named("claude")
        };
        let (id, name) = (&execution.plan.id, name.into());
        let zero = std::time::Duration::ZERO;
        (self.agents)
            .track_agent(daemon, id, &agent, &chat, &name, true, zero)
            .await;
        (chat, execution)
    }

    /// Record `state` as the workspace `id`'s, with `error`, as its
    /// lifecycle and as the daemon reads it.
    pub(crate) async fn set_state(&self, id: &SpaceId, state: WorkspaceState, error: Option<&str>) {
        let error = error.map(str::to_owned);
        (self.daemon.record_state(id, state, error.clone()))
            .await
            .unwrap();
        self.update_space(id, |space| {
            if let SpacePart::Workspace(workspace) = &mut space.part {
                workspace.state = state;
                workspace.error = error;
            }
        });
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
        let run = shoal_protocol::test_support::execution(id, space.as_str());
        (self.agents.store)
            .write(move |tx| seeds::execution(tx, &run, None))
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
async fn service(
    paths: Paths,
    config: Arc<ConfigState>,
    records: Arc<StandInRecords>,
    hosts: Vec<Arc<dyn Host>>,
) -> Agents {
    let store = shoal_store::test_support::unsynced_store(paths.database())
        .await
        .unwrap();
    let gates = Arc::new(Gates::default());
    let operations = shoal_daemon_base::operations::Operations::new(paths.clone(), gates.clone());
    // The queue's starts run no worker but note what they started.
    operations.fake_workers();
    Agents::new(
        store.clone(),
        gates,
        config,
        Arc::new(Signals::default()),
        Arc::new(Notifications::new(store.clone())),
        Arc::default(),
        Arc::new(operations),
        paths,
        records,
        Arc::new(StandInRemovals),
        hosts,
    )
}
