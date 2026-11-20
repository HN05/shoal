//! What the triggers and the runner need of the rest of the daemon, which
//! implements it: the registered repositories and their config, the agents
//! a trigger's launch meets and starts, the permission a scoped caller acts
//! with, the forges the runner registers on, and the agent queue's rows it
//! changes in its own transactions.
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::Connection;
use shoal_core::config::{Config, Effective, repo::ConfigLayers};
use shoal_daemon_base::callers::Caller;
use shoal_daemon_base::notifications::Coordinators;
use shoal_forge::account::Account;
use shoal_forge_types::{ForgeRepo, items::reference::ItemRef};
use shoal_ids::{
    execution_ids::ExecutionId, queue_entry_ids::QueueEntryId, repository_ids::RepositoryId,
    space_ids::SpaceId,
};
use shoal_protocol::{
    config::permissions::PermissionKey,
    events::StoppedProcess,
    launches::{Launch, LaunchSource},
    model::{QueueEntry, QueuedJob, Repository, Space, Workspace, WorkspaceUsage},
    protocol::ItemContext,
    space_names::{SpaceName, SpaceSelector},
};
use shoal_refs::CommitId;

/// The registered repositories, and the config the daemon reads for them.
pub trait Repositories: Send + Sync {
    /// The registered repositories.
    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>>;

    /// The registered repository `id`.
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>>;

    /// The config layers of the registered repository `id`: its
    /// checkout's file and its local config.
    fn repository_layers<'a>(&'a self, id: &'a RepositoryId)
    -> BoxFuture<'a, Result<ConfigLayers>>;

    /// The registered repository a request made in `context` acts in: a
    /// workspace's or a chat's, or the checkout's; none without a context
    /// or for a chat without a repository.
    fn context_repository(
        &self,
        context: Option<ItemContext>,
    ) -> BoxFuture<'_, Result<Option<RepositoryId>>>;

    /// The settings of the registered repository `id` under the global
    /// `config`.
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

/// The agents a trigger's launch starts, and those it waits for.
pub trait Agents: Send + Sync {
    /// The agent queue's entries, as the user sees them.
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

    /// Launch `launch` as the user, through the operation `shoal start`
    /// uses, from `source`: the space it opens.
    fn launch(
        self: Arc<Self>,
        launch: Launch,
        source: LaunchSource,
    ) -> BoxFuture<'static, Result<SpaceId>>;
}

/// The permissions scoped callers act with.
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

/// The forges the runner registers on and acts on, as the user.
pub trait Forges: Send + Sync {
    /// The account the user acts as on forges.
    fn user_account(&self) -> Account;

    /// The forge repository of `repository`'s checkout.
    fn checkout_forge<'a>(&'a self, repository: &'a Repository)
    -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The forge repository at the web URL `url`.
    fn resolve_forge<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The forge repository `item` belongs to.
    fn item_forge<'a>(&'a self, item: &'a ItemRef) -> BoxFuture<'a, Result<ForgeRepo>>;
}

/// The spaces a job's run is for, and those jobs run in.
pub trait Spaces: Send + Sync {
    /// The spaces this machine has.
    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>>;

    /// What the workspace `id` used at its latest usage sample.
    fn workspace_usage(&self, id: &SpaceId) -> Option<WorkspaceUsage>;

    /// The workspace `id`.
    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>>;

    /// The ID of the space `selector` names.
    fn select_space_id<'a>(&'a self, selector: &'a SpaceSelector)
    -> BoxFuture<'a, Result<SpaceId>>;
}

/// The executions of the runner's jobs.
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

    /// Stop the runner of the job workspace `id` for `reason`, saving
    /// nothing.
    fn stop_job_runner<'a>(&'a self, id: &'a SpaceId, reason: String) -> BoxFuture<'a, Result<()>>;
}

/// The workspaces the runner's jobs run in.
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
}

/// Everything the triggers and the runner need of the daemon, which each of
/// their operations that reaches beyond them takes.
pub trait Daemon:
    Repositories + Agents + Permissions + Forges + Spaces + Workspaces + Executions + Coordinators
{
}

impl<T> Daemon for T where
    T: Repositories
        + Agents
        + Permissions
        + Forges
        + Spaces
        + Workspaces
        + Executions
        + Coordinators
{
}

/// A job waiting in the agent queue for admission.
pub struct WaitingEntry {
    pub id: QueueEntryId,
    pub name: SpaceName,
    pub job: QueuedJob,
}

/// The agent queue's rows the runner changes in its own connection or
/// transaction. Its methods run on the store's thread, so they are not
/// futures.
pub trait Records: Send + Sync {
    /// Take the job entries of `repository` out of the agent queue.
    fn forget_job_entries(&self, db: &Connection, repository: &RepositoryId) -> Result<()>;

    /// Each waiting job's entry ID, repository and job.
    fn waiting_jobs(&self, db: &Connection)
    -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>>;

    /// `repository`'s job entries waiting for admission.
    fn waiting_job_entries(
        &self,
        db: &Connection,
        repository: &RepositoryId,
    ) -> Result<Vec<WaitingEntry>>;

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
