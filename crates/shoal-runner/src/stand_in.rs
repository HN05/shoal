//! A stand-in for the rest of the daemon, which the tests of the triggers
//! and the runner build the service over: one registered repository, whose
//! checkout's file, local config and forge a test gives, recorded in the
//! store as the runner's own records reference it, an agent queue and
//! agents a test sets, launches that each open a space whose agent then
//! runs, and the user's account from the global config.
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use futures_util::future::BoxFuture;
use rusqlite::Connection;
use shoal_core::config::{
    Config, Effective,
    forges::Whose,
    repo::{self, ConfigLayers},
};
use shoal_daemon_base::{
    alerts::{NewAlert, Sent},
    callers::Caller,
    config_state::ConfigState,
    gates::Gates,
    notifications::{Coordinators, Notifications, Source},
};
use shoal_forge::{
    RepoRequests,
    account::Account,
    hosts::{HostType, HostTypes},
    items::reference::ReferenceRequests,
};
use shoal_forge_types::{ForgeRepo, backends::ForgeKind, items::reference::ItemRef};
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    config::permissions::PermissionKey,
    launches::{Launch, LaunchSource},
    model::{QueueEntry, QueuedJob, Repository, Space, Workspace, WorkspaceUsage},
    paths::Paths,
    protocol::ItemContext,
};

use crate::{
    Runner,
    daemon::{Agents, Daemon, Forges, Permissions, Records, Repositories, Spaces, WaitingEntry},
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
        let config = shoal_core::test_support::global_config("");
        let daemon = Arc::new(StandIn {
            repository,
            config: Arc::new(ConfigState::new(config)),
            tracked: Mutex::new(String::new()),
            local: Mutex::new(local.to_owned()),
            contribution: AtomicBool::new(false),
            removing: AtomicBool::new(false),
            forge: Mutex::default(),
            launched: Mutex::default(),
            agents: Mutex::default(),
            queue: Mutex::default(),
            opened: Mutex::default(),
            forgotten: Mutex::default(),
            spaces: Mutex::default(),
        });
        let runner = runner(&root, &daemon).await;
        let record = daemon.repository.clone();
        (runner.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO repositories(id,path,source,last_used) VALUES (?1,?2,?3,1)",
                    rusqlite::params![record.id, record.path.to_str(), record.source],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        Self {
            root,
            runner,
            daemon,
        }
    }

    /// The service as a daemon started again over the same store has it.
    pub(crate) async fn restarted(&self) -> Arc<Runner> {
        runner(&self.root, &self.daemon).await
    }

    /// Make `text` the global config.
    pub(crate) fn configure(&self, text: &str) {
        let config = shoal_core::test_support::global_config(text);
        self.daemon.config.publish(config);
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

    /// Record a workspace of the repository named `name`, as a claimed
    /// job's runs in.
    pub(crate) async fn record_workspace(&self, name: &str) -> SpaceId {
        let (id, repository) = (SpaceId::generate(), self.repository().clone());
        let (row, name) = (id.clone(), name.to_owned());
        (self.runner.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO spaces(id,kind,repository_id,name) VALUES (?1,'workspace',?2,?3)",
                    rusqlite::params![row, repository, name],
                )?;
                tx.execute(
                    "INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
                     VALUES (?1,'/work/'||?2,?2,'ready','abc','/admin')",
                    rusqlite::params![row, name],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        id
    }
}

/// The service over the store in `root`, reading the config of `daemon`
/// and changing its queue rows.
async fn runner(root: &tempfile::TempDir, daemon: &Arc<StandIn>) -> Arc<Runner> {
    let path = root.path().join("state.sqlite3");
    let store = shoal_store::test_support::unsynced_store(path)
        .await
        .unwrap();
    Arc::new(Runner::new(
        store.clone(),
        Arc::new(Gates::default()),
        daemon.config.clone(),
        Arc::new(Notifications::new(store)),
        daemon.clone(),
        Paths::for_test(root.path()),
    ))
}

/// A daemon with one registered repository.
pub(crate) struct StandIn {
    repository: Repository,
    /// The global config, which the service reads too.
    config: Arc<ConfigState>,
    /// The repository's checkout's file.
    pub(crate) tracked: Mutex<String>,
    /// The repository's local config.
    pub(crate) local: Mutex<String>,
    /// Whether `shoal contribute` registered the repository.
    pub(crate) contribution: AtomicBool,
    /// Whether the repository is being removed.
    pub(crate) removing: AtomicBool,
    /// The web URL of the checkout's forge repository, and the forge every
    /// host serves.
    pub(crate) forge: Mutex<Option<(String, ForgeKind)>>,
    /// What each launch asked for, with the space it opened, oldest first.
    pub(crate) launched: Mutex<Vec<(SpaceId, Launch)>>,
    /// The spaces whose agent runs or waits.
    pub(crate) agents: Mutex<HashSet<SpaceId>>,
    /// The agent queue's entries that wait or start.
    pub(crate) queue: Mutex<HashSet<QueueEntryId>>,
    /// The space each queued launch opened, once it did.
    pub(crate) opened: Mutex<HashMap<QueueEntryId, SpaceId>>,
    /// The repositories whose job entries left the queue, in order.
    pub(crate) forgotten: Mutex<Vec<RepositoryId>>,
    /// The spaces this machine has.
    pub(crate) spaces: Mutex<Vec<Space>>,
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

    /// The repository's config layers: its checkout's file and its local
    /// config.
    fn layers(&self) -> Result<ConfigLayers> {
        let tracked = self.tracked.lock().unwrap().clone();
        let local = self.local.lock().unwrap().clone();
        Ok(ConfigLayers {
            worktree_file: repo::parse(&tracked)?,
            local_config: repo::parse(&local)?,
            ..Default::default()
        })
    }

    /// The forge every host serves.
    fn hosts(&self) -> Hosts {
        let forge = self.forge.lock().unwrap();
        Hosts((forge.as_ref()).map_or(ForgeKind::Forgejo, |(_, kind)| *kind))
    }
}

/// Every host serves the one forge.
struct Hosts(ForgeKind);

impl HostTypes for Hosts {
    async fn forge_type(&self, _host: &str, _scheme: &'static str) -> Result<HostType> {
        Ok(HostType::Settled(self.0))
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
        let layers = self.layers();
        Box::pin(async move { layers })
    }

    fn repository_settings_from<'a>(
        &'a self,
        config: &'a Config,
        _: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Effective>> {
        let layers = self.layers();
        Box::pin(async move { config.resolve(&layers?) })
    }

    fn ensure_repository_available<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<()>> {
        let removing = self.removing.load(Ordering::Relaxed);
        Box::pin(async move {
            anyhow::ensure!(!removing, "repository {id} is being removed");
            Ok(())
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
    /// The stand-in's queue lists no entries.
    fn queue_entries(&self) -> BoxFuture<'_, Result<Vec<QueueEntry>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

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

    /// A job could always start.
    fn job_could_start(&self) -> BoxFuture<'_, Result<bool>> {
        Box::pin(async { Ok(true) })
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

impl Forges for StandIn {
    fn user_account(&self) -> Account {
        Account::user(self.config.current().machine.forges.logins(Whose::User))
    }

    fn checkout_forge<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, Result<ForgeRepo>> {
        let url = (self.forge.lock().unwrap().as_ref()).map(|(url, _)| url.clone());
        Box::pin(async move {
            let url = url.context("the checkout has no forge remote")?;
            ForgeRepo::resolve(&url, &self.hosts()).await
        })
    }

    fn resolve_forge<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async move { ForgeRepo::resolve(url, &self.hosts()).await })
    }

    fn item_forge<'a>(&'a self, item: &'a ItemRef) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async move { item.forge(&self.hosts()).await })
    }
}

impl Spaces for StandIn {
    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>> {
        let spaces = self.spaces.lock().unwrap().clone();
        Box::pin(async move { Ok(spaces) })
    }

    /// No workspace has a usage sample.
    fn workspace_usage(&self, _: &SpaceId) -> Option<WorkspaceUsage> {
        None
    }

    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(async move { anyhow::bail!("no workspace {id}") })
    }
}

/// No space has a coordinator, so every event reaches the user.
impl Coordinators for StandIn {
    fn tell<'a>(&'a self, _: NewAlert, _: bool, _: Source) -> BoxFuture<'a, Option<Sent>> {
        Box::pin(async { None })
    }
}

/// The agent queue's rows, which the stand-in only records the runner
/// changing.
impl Records for StandIn {
    fn forget_job_entries(&self, _: &Connection, repository: &RepositoryId) -> Result<()> {
        self.forgotten.lock().unwrap().push(repository.clone());
        Ok(())
    }

    /// The stand-in's queue holds no jobs.
    fn waiting_job_entries(&self, _: &Connection, _: &RepositoryId) -> Result<Vec<WaitingEntry>> {
        Ok(Vec::new())
    }

    /// Every entry still waits.
    fn delete_waiting_entry(&self, _: &Connection, _: &QueueEntryId) -> Result<bool> {
        Ok(true)
    }

    /// The stand-in's queue holds no jobs.
    fn forget_unlisted_job_entries(
        &self,
        _: &Connection,
        _: &RepositoryId,
        _: &HashSet<String>,
    ) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }

    /// The stand-in's queue keeps no jobs.
    fn queue_job(&self, _: &Connection, _: &RepositoryId, _: &QueuedJob) -> Result<()> {
        Ok(())
    }

    /// The stand-in's queue holds no jobs.
    fn waiting_jobs(&self, _: &Connection) -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>> {
        Ok(Vec::new())
    }
}

/// A directory holding an executable `program`, as the runner finds a
/// forge's runner program on its `PATH`.
pub(crate) fn runner_program(root: &std::path::Path, program: &str) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join(program);
    std::fs::write(&path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin.into_os_string()
}
