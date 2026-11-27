//! A stand-in for the rest of the daemon, which the tests of the triggers
//! and the runner build the service over, with one registered repository.
use shoal_core::test_support::git;
use shoal_daemon_base::forge_access::ForgeAccess;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::Connection;
use shoal_core::config::{
    Config, Effective,
    repo::{self, ConfigLayers},
};
use shoal_daemon_base::{
    alerts::{NewAlert, Sent},
    callers::Caller,
    config_state::ConfigState,
    gates::Gates,
    notifications::{Coordinators, Notifications, Source},
};
use shoal_forge_types::backends::ForgeKind;
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    config::permissions::PermissionKey,
    hub::reruns::CutOffJob,
    launches::{Launch, LaunchSource},
    model::{QueueEntry, QueuedJob, Repository, Space, Workspace, WorkspaceUsage},
    paths::Paths,
    protocol::ItemContext,
};

use shoal_store::test_support::seeds;

use crate::{
    Runner,
    daemon::{Agents, Daemon, Permissions, Records, Repositories, Spaces, WaitingEntry},
};

/// The service over a stand-in daemon with one registered repository.
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) runner: Arc<Runner>,
    pub(crate) daemon: Arc<StandIn>,
}

impl Fixture {
    /// Make the repository's checkout one whose `origin` is the forge
    /// repository at `url`, on a host that serves `kind`.
    pub(crate) fn place_on_forge(&self, url: &str, kind: ForgeKind) {
        let path = &self.daemon.repository.path;
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "-q"]);
        git(path, &["remote", "add", "origin", url]);
        let host = url.split('/').nth(2).expect("a URL names its host");
        shoal_forge_types::hosts::learn(host, kind);
    }

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
            shared_url: None,
        };
        let config = shoal_core::test_support::global_config("");
        let daemon = Arc::new(StandIn {
            repository,
            config: Arc::new(ConfigState::new(config)),
            tracked: Mutex::new(String::new()),
            local: Mutex::new(local.to_owned()),
            contribution: AtomicBool::new(false),
            removing: AtomicBool::new(false),
            launched: Mutex::default(),
            agents: Mutex::default(),
            queue: Mutex::default(),
            opened: Mutex::default(),
            forgotten: Mutex::default(),
            spaces: Mutex::default(),
            could_start: AtomicBool::new(true),
            admitted: Mutex::default(),
            executions: Mutex::default(),
            handed: Mutex::default(),
            rank: Mutex::new(Ok(0)),
        });
        let runner = runner(&root, &daemon).await;
        let record = daemon.repository.clone();
        (runner.store)
            .write(move |tx| seeds::repository(tx, &record))
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

    pub(crate) fn configure(&self, text: &str) {
        let config = shoal_core::test_support::global_config(text);
        self.daemon.config.publish(config);
    }

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

    /// Record the claim `id` of `job` in a workspace of its own named
    /// `name`, whose runner launched at `launched_at` as `execution`.
    pub(crate) async fn record_claim(
        &self,
        id: &str,
        job: &QueuedJob,
        launched_at: Option<crate::time::UnixTime>,
        execution: Option<shoal_ids::execution_ids::ExecutionId>,
    ) -> crate::jobs::ClaimedJob {
        let workspace = self.record_workspace(id).await;
        let claim = crate::jobs::ClaimedJob {
            id: id.into(),
            repository_id: self.repository().clone(),
            workspace_id: workspace,
            execution_id: execution,
            launched_at,
            job: job.clone(),
            runner_id: None,
        };
        let row = claim.clone();
        let job = serde_json::to_string(job).unwrap();
        (self.runner.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO ci_jobs(id,repository_id,workspace_id,execution_id,job,started_at,launched_at)
                     VALUES (?1,?2,?3,?4,?5,0,?6)",
                    rusqlite::params![
                        row.id,
                        row.repository_id,
                        row.workspace_id,
                        row.execution_id,
                        job,
                        row.launched_at
                    ],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        claim
    }

    /// Record a workspace of the repository named `name`, as a claimed
    /// job's runs in.
    pub(crate) async fn record_workspace(&self, name: &str) -> SpaceId {
        let id = SpaceId::generate();
        let path = std::path::Path::new("/work").join(name);
        let workspace = shoal_protocol::test_support::workspace(
            id.as_str(),
            self.repository().as_str(),
            name,
            &path,
        );
        (self.runner.store)
            .write(move |tx| seeds::workspace(tx, &workspace))
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
    let gates = Arc::new(Gates::default());
    let forges = ForgeAccess::new(store.clone(), gates.clone(), daemon.config.clone());
    Arc::new(Runner::new(
        store.clone(),
        gates,
        daemon.config.clone(),
        Arc::new(Notifications::new(store)),
        Arc::new(forges),
        daemon.clone(),
        Paths::for_test(root.path()),
    ))
}

/// A daemon with one registered repository.
pub(crate) struct StandIn {
    pub(crate) repository: Repository,
    config: Arc<ConfigState>,
    pub(crate) tracked: Mutex<String>,
    pub(crate) local: Mutex<String>,
    /// Whether `shoal contribute` registered the repository.
    pub(crate) contribution: AtomicBool,
    pub(crate) removing: AtomicBool,
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
    pub(crate) spaces: Mutex<Vec<Space>>,
    /// Whether a job could start beside those in use.
    pub(crate) could_start: AtomicBool,
    pub(crate) admitted: Mutex<Vec<(QueueEntryId, RepositoryId, QueuedJob)>>,
    pub(crate) executions: Mutex<crate::daemon::RecordedExecutions>,
    /// The CI jobs cut off that the hub keeps, none for a machine in no
    /// hub.
    pub(crate) handed: Mutex<Option<Vec<CutOffJob>>>,
    /// This machine's rank for every job, or why the hub cannot give it.
    pub(crate) rank: Mutex<Result<usize, String>>,
}

impl StandIn {
    pub(crate) fn tasks(&self) -> Vec<Vec<String>> {
        let launched = self.launched.lock().unwrap();
        launched
            .iter()
            .map(|(_, launch)| launch.task.clone())
            .collect()
    }

    pub(crate) fn opened_spaces(&self) -> Vec<SpaceId> {
        let launched = self.launched.lock().unwrap();
        launched.iter().map(|(space, _)| space.clone()).collect()
    }

    pub(crate) fn run_agent(&self, id: &SpaceId) {
        self.agents.lock().unwrap().insert(id.clone());
    }

    pub(crate) fn stop_agent(&self, id: &SpaceId) {
        self.agents.lock().unwrap().remove(id);
    }

    fn layers(&self) -> Result<ConfigLayers> {
        let tracked = self.tracked.lock().unwrap().clone();
        let local = self.local.lock().unwrap().clone();
        Ok(ConfigLayers {
            worktree_file: repo::parse(&tracked)?,
            local_config: repo::parse(&local)?,
            ..Default::default()
        })
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

    fn wake_admission(&self) {}

    /// The stand-in's queue is never suspended.
    fn queue_suspended(&self) -> bool {
        false
    }

    fn lock_admission(&self) -> BoxFuture<'_, Box<dyn Send + '_>> {
        Box::pin(async { Box::new(()) as Box<dyn Send> })
    }

    fn job_could_start(&self) -> BoxFuture<'_, Result<bool>> {
        let could = self.could_start.load(Ordering::Relaxed);
        Box::pin(async move { Ok(could) })
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

impl crate::daemon::Hub for StandIn {
    fn hand_cut_off_jobs(&self, jobs: Vec<CutOffJob>) -> BoxFuture<'_, Result<bool>> {
        let mut handed = self.handed.lock().unwrap();
        let kept = handed.as_mut().map(|handed| handed.extend(jobs)).is_some();
        Box::pin(async move { Ok(kept) })
    }

    fn rank_job<'a>(&'a self, _: &'a Repository) -> BoxFuture<'a, Result<usize>> {
        let rank = self.rank.lock().unwrap().clone();
        Box::pin(async move { rank.map_err(anyhow::Error::msg) })
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

    /// A selector by ID names that space.
    fn select_space_id<'a>(
        &'a self,
        selector: &'a shoal_protocol::space_names::SpaceSelector,
    ) -> BoxFuture<'a, Result<SpaceId>> {
        Box::pin(async move {
            (selector.given_id().cloned()).ok_or_else(|| anyhow::anyhow!("no space {selector}"))
        })
    }
}

/// The stand-in runs no job processes.
impl crate::daemon::Executions for StandIn {
    fn stop_job_processes<'a>(
        &'a self,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<shoal_protocol::events::StoppedProcess>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn launch_job_command<'a>(
        &'a self,
        _: &'a Workspace,
        _: std::path::PathBuf,
        _: shoal_forge::ci::runner::JobCommand,
    ) -> BoxFuture<'a, Result<shoal_ids::execution_ids::ExecutionId>> {
        Box::pin(async { anyhow::bail!("the stand-in launches no job commands") })
    }

    fn interrupt_job_processes<'a>(
        &'a self,
        _: &'a SpaceId,
        first: BoxFuture<'a, Result<()>>,
    ) -> BoxFuture<'a, Result<Vec<shoal_protocol::events::StoppedProcess>>> {
        Box::pin(async {
            first.await?;
            Ok(Vec::new())
        })
    }

    fn stop_job_runner<'a>(&'a self, _: &'a SpaceId, _: String) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn execution_recorded<'a>(
        &'a self,
        _: &'a shoal_ids::execution_ids::ExecutionId,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn workspace_has_executions<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn job_execution_runs<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'a shoal_ids::execution_ids::ExecutionId,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn workspace_execution<'a>(
        &'a self,
        _: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<shoal_ids::execution_ids::ExecutionId>>> {
        Box::pin(async { Ok(None) })
    }
}

/// The stand-in's job workspaces never move.
impl crate::daemon::Workspaces for StandIn {
    fn move_job_workspace<'a>(
        &'a self,
        id: &'a SpaceId,
        _: &'a shoal_refs::CommitId,
        _: Option<&'a str>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { anyhow::bail!("job workspace {id} cannot move") })
    }

    fn remove_job_workspace<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn record_job_workspace<'a>(
        &'a self,
        _: &'a RepositoryId,
        _: (&'a str, shoal_protocol::space_names::SpaceName),
        _: &'a shoal_refs::CommitId,
        _: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn crate::daemon::RecordedJobWorkspace<'a> + 'a>>> {
        Box::pin(async { anyhow::bail!("the stand-in records no job workspaces") })
    }

    fn workspace_settings<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move { self.config.current().resolve(&self.layers()?) })
    }

    fn hold_permit<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(true) })
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

    fn admitted_jobs(
        &self,
        _: &Connection,
    ) -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>> {
        Ok(self.admitted.lock().unwrap().clone())
    }

    fn recorded_executions(&self, _: &Connection) -> Result<crate::daemon::RecordedExecutions> {
        Ok(self.executions.lock().unwrap().clone())
    }

    fn remove_queue_entry(&self, _: &Connection, _: &QueueEntryId) -> Result<()> {
        Ok(())
    }

    fn return_job_entry(&self, _: &Connection, _: &QueueEntryId) -> Result<()> {
        Ok(())
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
