//! The interfaces through which the triggers and the runner reach the rest
//! of the daemon.
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::Connection;
use shoal_core::config::{Config, Effective, repo::ConfigLayers};
use shoal_daemon_base::callers::Caller;
use shoal_daemon_base::notifications::Coordinators;
use shoal_forge::ci::runner::JobCommand;
use shoal_ids::{
    execution_ids::ExecutionId, queue_entry_ids::QueueEntryId, repository_ids::RepositoryId,
    space_ids::SpaceId,
};
use shoal_protocol::{
    config::permissions::PermissionKey,
    events::StoppedProcess,
    hub::reruns::CutOffJob,
    launches::{Launch, LaunchSource},
    model::{QueueEntry, QueuedJob, Repository, Space, Workspace, WorkspaceUsage},
    protocol::ItemContext,
    space_names::{SpaceName, SpaceSelector},
};
use shoal_refs::CommitId;

pub trait Repositories: Send + Sync {
    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>>;

    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>>;

    fn repository_layers<'a>(&'a self, id: &'a RepositoryId)
    -> BoxFuture<'a, Result<ConfigLayers>>;

    /// The repository a request made in `context` acts in: none without a
    /// context, or for a chat without a repository.
    fn context_repository(
        &self,
        context: Option<ItemContext>,
    ) -> BoxFuture<'_, Result<Option<RepositoryId>>>;

    fn repository_settings_from<'a>(
        &'a self,
        config: &'a Config,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// Refuse the registered repository `id` while it is being removed.
    fn ensure_repository_available<'a>(&'a self, id: &'a RepositoryId)
    -> BoxFuture<'a, Result<()>>;

    /// Whether this daemon acts on what the global config sets up for the
    /// machine, such as its global triggers and its webhook listener.
    fn acts_on_global_config(&self) -> bool;
}

pub trait Agents: Send + Sync {
    fn queue_entries(&self) -> BoxFuture<'_, Result<Vec<QueueEntry>>>;

    /// The agent queue's entries that wait or start, not those that failed.
    fn live_queue_entries(&self) -> BoxFuture<'_, Result<HashSet<QueueEntryId>>>;

    /// The space the queued launch `entry` opened, once it did.
    fn started_launch<'a>(
        &'a self,
        entry: &'a QueueEntryId,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>>;

    /// The spaces whose agent runs or waits in the agent queue.
    fn agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>>;

    /// Whether some CI job could start now beside the agents and jobs in
    /// use.
    fn job_could_start(&self) -> BoxFuture<'_, Result<bool>>;

    /// Have admission look at the agent queue again, as once a claim ends.
    fn wake_admission(&self);

    /// Whether the agent queue holds admission, as while the system shuts
    /// down or the machine sleeps.
    fn queue_suspended(&self) -> bool;

    /// Hold the agent queue's admission until the guard drops, so a
    /// suspension waits for what admission records meanwhile.
    fn lock_admission(&self) -> BoxFuture<'_, Box<dyn Send + '_>>;

    /// Launch `launch` as the user, through the operation `shoal start`
    /// uses, from `source`: the space it opens.
    fn launch(
        self: Arc<Self>,
        launch: Launch,
        source: LaunchSource,
    ) -> BoxFuture<'static, Result<SpaceId>>;
}

pub trait Permissions: Send + Sync {
    /// Let `caller` take `action`, which `key` governs, in `repository`:
    /// an error refuses it, or makes it an approval request.
    fn authorize<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        key: PermissionKey,
        action: String,
        repository: Option<RepositoryId>,
    ) -> BoxFuture<'a, Result<()>>;
}

pub trait Spaces: Send + Sync {
    /// The spaces this machine has.
    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>>;

    /// What the workspace `id` used at its latest usage sample.
    fn workspace_usage(&self, id: &SpaceId) -> Option<WorkspaceUsage>;

    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>>;

    fn select_space_id<'a>(&'a self, selector: &'a SpaceSelector)
    -> BoxFuture<'a, Result<SpaceId>>;
}

pub trait Executions: Send + Sync {
    /// Stop every process of the job workspace `id` but its runner,
    /// leftovers whose parent is gone included, returning what it stopped.
    fn stop_job_processes<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<StoppedProcess>>>;

    /// Whether the execution `id` of a job in the workspace `workspace` may
    /// still run, which the daemon proves from its processes.
    fn job_execution_runs<'a>(
        &'a self,
        workspace: &'a SpaceId,
        id: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<bool>>;

    /// An execution recorded in the workspace `id`, if any.
    fn workspace_execution<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<ExecutionId>>>;

    /// Whether the daemon still records the execution `id`, running or
    /// awaiting reattachment.
    fn execution_recorded<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Result<bool>>;

    /// Whether the workspace `id` has any recorded execution, in any state.
    fn workspace_has_executions<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// Run `command` in `workspace` as a tracked execution that outlives
    /// the daemon, and the service it runs as, with its output in `log`,
    /// returning the execution's ID.
    fn launch_job_command<'a>(
        &'a self,
        workspace: &'a Workspace,
        log: PathBuf,
        command: JobCommand,
    ) -> BoxFuture<'a, Result<ExecutionId>>;

    /// Stop every process of the job workspace `id` but its runner, as
    /// [`Self::stop_job_processes`] does. It reserves the workspace's lifecycle
    /// and runs `first` before it signals anything, so another lifecycle
    /// operation refuses both.
    fn interrupt_job_processes<'a>(
        &'a self,
        id: &'a SpaceId,
        first: BoxFuture<'a, Result<()>>,
    ) -> BoxFuture<'a, Result<Vec<StoppedProcess>>>;

    /// Stop the runner of the job workspace `id` for `reason`, saving
    /// nothing.
    fn stop_job_runner<'a>(&'a self, id: &'a SpaceId, reason: String) -> BoxFuture<'a, Result<()>>;
}

pub trait Workspaces: Send + Sync {
    /// Move the job workspace `id` to `commit`, fetched by `reference` when
    /// it is not there yet.
    fn move_job_workspace<'a>(
        &'a self,
        id: &'a SpaceId,
        commit: &'a CommitId,
        reference: Option<&'a str>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Remove the job workspace `id` whatever it holds; its claim goes with
    /// it.
    fn remove_job_workspace<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>>;

    /// Record a job workspace of `repository` at `commit`, before it is checked
    /// out. `names` gives its directory and name, and `reference` fetches the
    /// commit when it is missing. The workspace holds the repository's Git gate
    /// until it is checked out.
    fn record_job_workspace<'a>(
        &'a self,
        repository: &'a RepositoryId,
        names: (&'a str, SpaceName),
        commit: &'a CommitId,
        reference: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn RecordedJobWorkspace<'a> + 'a>>>;

    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// Take one lease named `name` from `pool` for the job workspace `id`,
    /// returning whether the pool had one; a pool that needs approval
    /// fails, since a job cannot ask for it.
    fn hold_permit<'a>(
        &'a self,
        id: &'a SpaceId,
        pool: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<bool>>;
}

/// A job workspace recorded and not yet checked out, which holds its
/// repository's Git gate until it is or it drops.
pub trait RecordedJobWorkspace<'a>: Send {
    fn id(&self) -> &SpaceId;

    fn check_out(self: Box<Self>) -> BoxFuture<'a, Result<Workspace>>;
}

pub trait Hub: Send + Sync {
    /// Hand the hub `jobs`, which this machine cut off, to rerun in its
    /// place, returning whether it keeps them now: false on the hub's own
    /// machine and on one in no hub.
    fn hand_cut_off_jobs(&self, jobs: Vec<CutOffJob>) -> BoxFuture<'_, Result<bool>>;
}

pub trait Daemon:
    Repositories + Agents + Permissions + Spaces + Workspaces + Executions + Hub + Coordinators
{
}

impl<T> Daemon for T where
    T: Repositories + Agents + Permissions + Spaces + Workspaces + Executions + Hub + Coordinators
{
}

/// The executions the daemon records: their IDs and the spaces they run in.
#[derive(Debug, Clone, Default)]
pub struct RecordedExecutions {
    pub ids: HashSet<ExecutionId>,
    pub spaces: HashSet<SpaceId>,
}

pub struct WaitingEntry {
    pub id: QueueEntryId,
    pub name: SpaceName,
    pub job: QueuedJob,
}

/// The agent queue's rows the runner changes in its own transaction. Its
/// methods run on the store's thread, so they return no futures.
pub trait Records: Send + Sync {
    fn forget_job_entries(&self, db: &Connection, repository: &RepositoryId) -> Result<()>;

    fn waiting_jobs(&self, db: &Connection)
    -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>>;

    fn waiting_job_entries(
        &self,
        db: &Connection,
        repository: &RepositoryId,
    ) -> Result<Vec<WaitingEntry>>;

    fn admitted_jobs(
        &self,
        db: &Connection,
    ) -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>>;

    /// The executions the daemon records, running or awaiting
    /// reattachment.
    fn recorded_executions(&self, db: &Connection) -> Result<RecordedExecutions>;

    fn remove_queue_entry(&self, db: &Connection, id: &QueueEntryId) -> Result<()>;

    /// Put the admitted entry `id` back in the queue, where it waits again.
    fn return_job_entry(&self, db: &Connection, id: &QueueEntryId) -> Result<()>;

    /// Delete the entry `id` if it still waits, returning whether it did.
    fn delete_waiting_entry(&self, db: &Connection, id: &QueueEntryId) -> Result<bool>;

    /// Delete `repository`'s waiting job entries whose handles are not
    /// among `listed`, returning the handles of those it keeps: listed,
    /// or admitted.
    fn forget_unlisted_job_entries(
        &self,
        db: &Connection,
        repository: &RepositoryId,
        listed: &HashSet<String>,
    ) -> Result<HashSet<String>>;

    /// Queue `job` of `repository` behind the work already waiting; its
    /// rank decides when it starts.
    fn queue_job(&self, db: &Connection, repository: &RepositoryId, job: &QueuedJob) -> Result<()>;
}
