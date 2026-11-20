//! A stand-in for the rest of the daemon, which the tests of the triggers
//! build the service over: one registered repository, whose checkout's
//! file and local config a test gives, an agent queue and agents a test
//! sets, and launches that each open a space whose agent then runs.
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_core::config::repo::{self, ConfigLayers};
use shoal_daemon_base::{callers::Caller, config_state::ConfigState, notifications::Notifications};
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    config::permissions::PermissionKey,
    launches::{Launch, LaunchSource},
    model::Repository,
    protocol::ItemContext,
};

use crate::{
    Runner,
    daemon::{Agents, Daemon, Permissions, Repositories},
};

/// The service over a stand-in daemon with one registered repository.
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) runner: Arc<Runner>,
    pub(crate) daemon: Arc<StandIn>,
}

impl Fixture {
    /// A registered repository whose local config is `local`, under a
    /// global config without triggers.
    pub(crate) async fn new(local: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let runner = runner(&root).await;
        let path = root.path().join("repo");
        let repository = Repository {
            id: "repo".into(),
            source: path.display().to_string(),
            path,
            last_used: 1,
            name: None,
            workspaces_dir: root.path().join("workspaces"),
            contribution: false,
        };
        let daemon = Arc::new(StandIn {
            repository,
            tracked: Mutex::new(String::new()),
            local: Mutex::new(local.to_owned()),
            contribution: AtomicBool::new(false),
            launched: Mutex::default(),
            agents: Mutex::default(),
            queue: Mutex::default(),
            opened: Mutex::default(),
        });
        Self {
            root,
            runner,
            daemon,
        }
    }

    /// The service as a daemon started again over the same store has it.
    pub(crate) async fn restarted(&self) -> Arc<Runner> {
        runner(&self.root).await
    }

    /// Make `text` the global config.
    pub(crate) fn configure(&self, text: &str) {
        let config = shoal_core::test_support::global_config(text);
        self.runner.config.publish(config);
    }

    /// The daemon the service's operations take.
    pub(crate) fn daemon(&self) -> Arc<dyn Daemon> {
        self.daemon.clone()
    }

    /// The context of a request made in the repository's checkout.
    pub(crate) fn context(&self) -> Option<ItemContext> {
        Some(ItemContext::Checkout(self.daemon.repository.id.clone()))
    }

    pub(crate) fn repository(&self) -> &RepositoryId {
        &self.daemon.repository.id
    }
}

/// The service over the store in `root`.
async fn runner(root: &tempfile::TempDir) -> Arc<Runner> {
    let path = root.path().join("state.sqlite3");
    let store = shoal_store::test_support::unsynced_store(path)
        .await
        .unwrap();
    let config = shoal_core::test_support::global_config("");
    Arc::new(Runner::new(
        store.clone(),
        Arc::new(ConfigState::new(config)),
        Arc::new(Notifications::new(store)),
    ))
}

/// A daemon with one registered repository.
pub(crate) struct StandIn {
    repository: Repository,
    /// The repository's checkout's file.
    pub(crate) tracked: Mutex<String>,
    /// The repository's local config.
    pub(crate) local: Mutex<String>,
    /// Whether `shoal contribute` registered the repository.
    pub(crate) contribution: AtomicBool,
    /// What each launch asked for, with the space it opened, oldest first.
    pub(crate) launched: Mutex<Vec<(SpaceId, Launch)>>,
    /// The spaces whose agent runs or waits.
    pub(crate) agents: Mutex<HashSet<SpaceId>>,
    /// The agent queue's entries that wait or start.
    pub(crate) queue: Mutex<HashSet<QueueEntryId>>,
    /// The space each queued launch opened, once it did.
    pub(crate) opened: Mutex<HashMap<QueueEntryId, SpaceId>>,
}

impl StandIn {
    /// The tasks of the launches, oldest first.
    pub(crate) fn tasks(&self) -> Vec<Vec<String>> {
        let launched = self.launched.lock().unwrap();
        launched
            .iter()
            .map(|(_, launch)| launch.task.clone())
            .collect()
    }

    /// The spaces the launches opened, oldest first.
    pub(crate) fn opened_spaces(&self) -> Vec<SpaceId> {
        let launched = self.launched.lock().unwrap();
        launched.iter().map(|(space, _)| space.clone()).collect()
    }

    /// Give the space `id` an agent that runs.
    pub(crate) fn run_agent(&self, id: &SpaceId) {
        self.agents.lock().unwrap().insert(id.clone());
    }

    /// Stop the agent of the space `id`.
    pub(crate) fn stop_agent(&self, id: &SpaceId) {
        self.agents.lock().unwrap().remove(id);
    }
}

impl Repositories for StandIn {
    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        let mut repository = self.repository.clone();
        repository.contribution = self.contribution.load(Ordering::Relaxed);
        Box::pin(async move { Ok(vec![repository]) })
    }

    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        Box::pin(async move {
            let repositories = self.repositories().await?;
            (repositories.into_iter())
                .find(|repository| repository.id == *id)
                .ok_or_else(|| anyhow::anyhow!("no repository {id}"))
        })
    }

    fn repository_layers<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<ConfigLayers>> {
        let tracked = self.tracked.lock().unwrap().clone();
        let local = self.local.lock().unwrap().clone();
        Box::pin(async move {
            Ok(ConfigLayers {
                worktree_file: repo::parse(&tracked)?,
                local_config: repo::parse(&local)?,
                ..Default::default()
            })
        })
    }

    /// Every request made with a context acts in the repository.
    fn context_repository(
        &self,
        context: Option<ItemContext>,
    ) -> BoxFuture<'_, Result<Option<RepositoryId>>> {
        let repository = context.map(|_| self.repository.id.clone());
        Box::pin(async move { Ok(repository) })
    }

    fn acts_on_global_config(&self) -> bool {
        true
    }
}

impl Agents for StandIn {
    fn live_queue_entries(&self) -> BoxFuture<'_, Result<HashSet<QueueEntryId>>> {
        let queue = self.queue.lock().unwrap().clone();
        Box::pin(async move { Ok(queue) })
    }

    fn started_launch<'a>(
        &'a self,
        entry: &'a QueueEntryId,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        let opened = self.opened.lock().unwrap().get(entry).cloned();
        Box::pin(async move { Ok(opened) })
    }

    fn agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        let agents = self.agents.lock().unwrap().clone();
        Box::pin(async move { Ok(agents) })
    }

    fn launch(
        self: Arc<Self>,
        launch: Launch,
        _: LaunchSource,
    ) -> BoxFuture<'static, Result<SpaceId>> {
        Box::pin(async move {
            let space = SpaceId::generate();
            self.launched.lock().unwrap().push((space.clone(), launch));
            self.run_agent(&space);
            Ok(space)
        })
    }
}

/// The user, whose every action goes ahead.
impl Permissions for StandIn {
    fn authorize<'a>(
        &'a self,
        _: Option<&'a Caller>,
        _: PermissionKey,
        _: String,
        _: Option<RepositoryId>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}
