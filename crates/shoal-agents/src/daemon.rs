//! What the agents need of the rest of the daemon, which implements it: the
//! spaces they run in, their settings, lifecycles and hooks, the
//! repositories whose paths they trust, the alerts that tell of them, what
//! else ends with an agent's turn, the operations that start and prepare
//! spaces, the merges an execution runs, the demos and viewers of
//! executions, the links, CI jobs, leases and reviews that follow an
//! agent's work, the conversions that hand an agent over, the snapshot the
//! queue ranks, and the records of other subsystems they read and write in
//! their own transactions.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_core::{
    config::{Effective, repo::ConfigLayers},
    hooks::HookKind,
    process::load::WorkLimit,
};
use shoal_daemon_base::{
    alerts::{About, AlertKind},
    callers::Caller,
    jobs::{JobCost, JobStop},
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
    item_numbers::ItemNumber,
    model::{
        Chat, Completion, MergeOptions, MergePlan, OpenedWorkspace, PortReservation,
        PreparationWait, QueueEntry, QueueRequest, Relation, Repository, Space, SpaceKind,
        UnmergePlan, Workspace, WorkspaceUsage,
    },
    progress::Progress,
    protocol::ScopeMemory,
    repository::Selector,
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

use crate::{capacity::Activity, execution_connection::RecordChange, model::QueuedJob};

/// The spaces agents run in.
pub trait Spaces: Send + Sync {
    /// The workspace or chat `selector` names, by ID or name.
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>>;

    /// The workspace or chat with the ID `id`.
    fn space_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>>;

    /// Whether the space `id` exists.
    fn space_exists<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// The spaces of this machine.
    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>>;

    /// The spaces `caller`'s scope reaches, every space for the user.
    fn visible_spaces<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<Space>>>;

    /// Refuse `caller` unless `space` names a space below its own, which
    /// it then names by ID; `refusal` says why otherwise.
    fn workspace_below<'a>(
        &'a self,
        caller: &'a Caller,
        space: &'a mut SpaceSelector,
        refusal: &'a str,
    ) -> BoxFuture<'a, Result<()>>;

    /// The queue entries `caller`'s scope reaches, every entry for the user.
    fn reachable_queue<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<QueueEntry>>>;

    /// The agent the space `id` runs, or last ran, as recorded.
    fn agent_of<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Agent>>>;

    /// The settings of `space`, as its config layers resolve them.
    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>>;

    /// The settings of `workspace`, as its config layers resolve them.
    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// [`Spaces::workspace_settings`] with the templates of its modes.
    fn workspace_settings_with_templates<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// The workspace `selector` names, refusing a chat.
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>>;

    /// The worktree paths of every workspace.
    fn workspace_paths(&self) -> BoxFuture<'_, Result<Vec<PathBuf>>>;

    /// The workspace a chat in it reads, for a chat in a workspace's
    /// worktree.
    fn chat_workspace<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Workspace>>;

    /// The chat with the ID `id`, while it exists.
    fn chat_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Chat>>>;

    /// The repository `chat` runs in, when it has one.
    fn chat_repository<'a>(&'a self, chat: &'a Chat) -> BoxFuture<'a, Result<Option<Repository>>>;

    /// The checkout `chat` runs in.
    fn chat_checkout_path<'a>(&'a self, chat: &'a Chat) -> BoxFuture<'a, Result<PathBuf>>;

    /// The temporary directory of `chat`, of `repository` where it has one.
    fn chat_tmp_dir(&self, repository: Option<&Repository>, chat: &Chat)
    -> Result<Option<PathBuf>>;

    /// The completion `shoal done` recorded for the space `id`.
    fn completion<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Completion>>>;

    /// Withdraw the completion of the space `id`, whose agent resumes.
    fn reopen_for_resume<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()>;

    /// Whether the space `id` is blocked.
    fn space_blocked<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// The progress of the space `selector` names.
    fn space_progress<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<Progress>>>;

    /// The priority a launch of `request` in `repository` takes.
    fn launch_priority<'a>(
        &'a self,
        request: &'a QueueRequest,
        repository: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Option<Priority>>>;

    /// Refuse a scoped `giver` the permissions `change` gives an agent in
    /// `space` beyond what its own space holds.
    fn ensure_change_held<'a>(
        &'a self,
        giver: Option<&'a Caller>,
        space: &'a Space,
        change: &'a PermissionChange,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Refuse a scoped `giver` an agent in the space `space` names whose
    /// `mode` and `given` permissions exceed what its own space holds.
    fn ensure_agent_held<'a>(
        &'a self,
        giver: Option<&'a Caller>,
        space: &'a SpaceSelector,
        mode: &'a ModeName,
        given: &'a PermissionsConfig,
    ) -> BoxFuture<'a, Result<()>>;

    /// Refuse `space` while it cannot be read as recorded, such as a
    /// workspace whose worktree moved.
    fn verify_space<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>>;

    /// Record that the space `id` was used now.
    fn touch<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()>;

    /// Run `hooks.post_agent_exit` where the agent of `space` worked, for
    /// `agent`, which exited with `exit_code` and, as `complete` says, ended
    /// its work, unless a removal has the space, which runs its own hooks.
    fn run_agent_exit_hook<'a>(
        &'a self,
        space: &'a Space,
        agent: &'a Agent,
        exit_code: Option<i32>,
        complete: bool,
    ) -> BoxFuture<'a, Result<()>>;

    /// The hook of `kind` `workspace` runs, when it has one.
    fn workspace_hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        kind: HookKind,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>>;

    /// Keep a checkpoint of the workspace `id` after its agent exited.
    fn checkpoint_after_agent<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()>;

    /// The memory and CPU the workspace `id` used last.
    fn workspace_usage(&self, id: &SpaceId) -> Option<WorkspaceUsage>;
}

/// The lifecycles of workspaces, which an execution waits out and holds.
pub trait Lifecycles: Send + Sync {
    /// Record `state`, and `error`, for the workspace `id`.
    fn set_state<'a>(
        &'a self,
        id: &'a SpaceId,
        state: WorkspaceState,
        error: Option<String>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Hold the workspace `id` in `state`, returning the state it had.
    fn reserve_lifecycle<'a>(
        &'a self,
        id: &'a SpaceId,
        state: WorkspaceState,
    ) -> BoxFuture<'a, Result<WorkspaceState>>;

    /// Whether a lifecycle operation held the workspace `id` since `seen`.
    fn reserved_since<'a>(
        &'a self,
        id: &'a SpaceId,
        seen: Option<Generation>,
    ) -> BoxFuture<'a, Result<bool>>;

    /// How many times the lifecycle of the workspace `id` changed.
    fn lifecycle_generation<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Generation>>;

    /// Refuse the workspace `id` while a lifecycle operation holds it.
    fn check_lifecycle<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>>;

    /// Wait until no lifecycle operation that passes holds the workspace
    /// `id`, refusing once `wanted` does; the generation it waited for.
    fn await_reservation_end<'a>(
        &'a self,
        id: &'a SpaceId,
        wanted: &'a (dyn Fn() -> Result<()> + Send + Sync),
    ) -> BoxFuture<'a, Result<Generation>>;

    /// Refuse the workspace `id` while a rename of it is unfinished.
    fn ensure_no_pending_rename<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>>;

    /// Refuse `workspace` unless its worktree is verifiably its own.
    fn verify_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>>;

    /// Reopen the workspace `selector` names, recreating its worktree.
    fn reopen_workspace<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<OpenedWorkspace>>;

    /// Bring `workspace`, which an agent is admitted to, up to date.
    fn refresh_admitted<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, ()>;
}

/// The operations that start and prepare spaces.
pub trait Operations: Send + Sync {
    /// Start the preparations that waited for what now cleared.
    fn start_waiting_preparations(self: Arc<Self>) -> BoxFuture<'static, Result<()>>;

    /// Tell whoever receives the events of `space` of the `warnings` its
    /// start left.
    fn tell_start_warnings<'a>(
        &'a self,
        space: &'a Space,
        warnings: &'a [String],
    ) -> BoxFuture<'a, ()>;

    /// Whether an operation still pushes what the user approved, by its
    /// name.
    fn pushing_operations(&self) -> Box<dyn Fn(&str) -> bool + Send + Sync>;

    /// The spaces an operation works in.
    fn operations_work_in(&self) -> HashSet<SpaceId>;

    /// Whether an operation runs in the space `id`, whose setup or merge
    /// its executions are.
    fn operation_runs_in(&self, id: &SpaceId) -> bool;

    /// Whether the daemon starts fake workers, as a test daemon does.
    fn fakes_workers(&self) -> bool;
}

/// The registered repositories, whose paths agents trust.
pub trait Repositories: Send + Sync {
    /// The registered repository `id`.
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>>;

    /// The registered repository `selector` names.
    fn selected_repository<'a>(
        &'a self,
        selector: &'a Selector,
    ) -> BoxFuture<'a, Result<Repository>>;

    /// The settings of the repository `id`.
    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>>;

    /// The config layers of the repository `id`.
    fn repository_layers<'a>(&'a self, id: &'a RepositoryId)
    -> BoxFuture<'a, Result<ConfigLayers>>;
}

/// The alerts that tell of agents.
pub trait Alerts: Send + Sync {
    /// Tell whoever receives the events of the space named `name`, or the
    /// user without one, of `message`. A failure only logs.
    fn notify<'a>(
        &'a self,
        name: Option<&'a SpaceName>,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()>;

    /// [`Alerts::notify`] about what `about` names.
    fn notify_about(&self, about: About, kind: AlertKind, message: String) -> BoxFuture<'_, ()>;

    /// Tell whoever receives the events of the space `id`, which has `name`,
    /// of `message`. A failure only logs.
    fn notify_space<'a>(
        &'a self,
        name: &'a SpaceName,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()>;

    /// Tell the agent of the space `id`, or whoever receives its events
    /// without one, of `message`; whether an agent heard it.
    fn tell_space<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, bool>;

    /// Announce `request` to whoever decides it.
    fn notify_approval<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()>;

    /// Ask the user about `requests`, which no space decides any longer.
    fn ask_user_about<'a>(&'a self, requests: &'a [ApprovalRequest]) -> BoxFuture<'a, ()>;

    /// Tell the coordinator of the space `id` that it is still blocked.
    fn tell_still_blocked<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()>;

    /// Tell the user that a state report of the agent of the space
    /// `selector` names failed with `error`.
    fn report_state_failure<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        error: &'a str,
    ) -> BoxFuture<'a, Result<()>>;
}

/// What else ends with an agent's turn.
pub trait Turns: Send + Sync {
    /// Post the `final_message` of the turn the agent `execution` runs in
    /// the space `selector` names ended where its turn marked it to go.
    fn end_marked_turn<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        execution: Option<&'a ExecutionId>,
        final_message: Option<String>,
    ) -> BoxFuture<'a, ()>;
}

/// The merges and unmerges an execution runs.
pub trait Merges: Send + Sync {
    /// The merge of the space `selector` names under `options`.
    fn prepare_merge<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        options: &'a MergeOptions,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<MergePlan>>;

    /// The unmerge of the space `selector` names.
    fn prepare_unmerge<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<UnmergePlan>>;

    /// Refuse the scoped `caller` the merge `plan` its permission does not
    /// let it run.
    fn authorize_scoped_merge<'a>(
        &'a self,
        plan: &'a MergePlan,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<()>>;

    /// Refuse the scoped `caller` the unmerge `plan`.
    fn authorize_scoped_unmerge<'a>(
        &'a self,
        plan: &'a UnmergePlan,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<()>>;

    /// Bring the merge records in line with what the execution behind
    /// `change` left the default branch at.
    fn update_merge_records<'a>(&'a self, change: &'a RecordChange) -> BoxFuture<'a, ()>;

    /// Follow the CI runs of the merge behind `change`, once pushed.
    fn follow_pushed_merge_runs<'a>(&'a self, change: &'a RecordChange) -> BoxFuture<'a, ()>;
}

/// The demos executions run and the viewers of their simulators.
pub trait Demos: Send + Sync {
    /// The demos running now.
    fn running_demos(&self) -> BoxFuture<'_, Result<Vec<RunningDemo>>>;

    /// The process group of the viewer the demo of `execution` runs.
    fn viewer_group<'a>(&'a self, execution: &'a ExecutionId)
    -> BoxFuture<'a, Result<Option<u32>>>;

    /// Stop the viewer the demo of `execution` ran.
    fn retire_execution_viewer<'a>(
        &'a self,
        execution: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<()>>;

    /// The exit code the demo of `execution` reported, else `exit_code`.
    fn reported_end<'a>(
        &'a self,
        execution: &'a ExecutionId,
        exit_code: Option<i32>,
    ) -> BoxFuture<'a, Option<i32>>;

    /// The launch of the demo `execution` never ran, which a start waits on.
    fn unrun_demo_launch<'a>(
        &'a self,
        execution: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<Option<String>>>;

    /// Restart the command of the demo `execution` runs; whether it did.
    fn restart_demo_command<'a>(
        &'a self,
        execution: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<bool>>;

    /// The lock a viewer's spawn holds for `execution`.
    fn viewer_spawn_lock(&self, execution: &ExecutionId) -> Arc<tokio::sync::Mutex<()>>;

    /// Forget the viewer locks of `execution`, which ended.
    fn end_viewer(&self, execution: &ExecutionId);
}

/// The links, watches and merge state that follow an agent's work.
pub trait Links: Send + Sync {
    /// Settle what the agent of the space `id` follows once it changed.
    fn settle_agent(&self, id: &SpaceId);

    /// Whether the updates the agent of the space `id` follows are recorded.
    fn agent_updates_recorded<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// Whether updates wait for the agent of the space `selector` names.
    fn updates_waiting<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<bool>>;

    /// Restart the watches of the space `id` until `stop`.
    fn restart_watches<'a>(
        &'a self,
        id: &'a SpaceId,
        stop: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<()>>;

    /// The HEAD of `workspace` now.
    fn current_head<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<CommitId>>;

    /// Link the item `item` names to the space `selector` names with
    /// `relation`, as `caller` asked; the warnings it left.
    fn link_item<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        kind: LinkKind,
        item: &'a ItemInput,
        relation: Relation,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<String>>>;
}

/// The CI jobs this machine runs, which the agent queue admits.
pub trait Runner: Send + Sync {
    /// The execution of each claimed job whose runner started, by the
    /// workspace it runs in.
    fn job_executions(&self) -> BoxFuture<'_, Result<HashMap<SpaceId, ExecutionId>>>;

    /// The claimed jobs whose runner's execution runs, by the space each
    /// runs in, with when it started.
    fn running_jobs(&self) -> BoxFuture<'_, Result<Vec<RunningJob>>>;

    /// Whether a job runs in the space `id`.
    fn runs_job<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, bool>;

    /// Wake the runner whose job the execution `id` ran.
    fn wake_runner_for_ended_job<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, ()>;

    /// The workspaces jobs run in.
    fn job_workspaces(&self) -> BoxFuture<'_, HashSet<SpaceId>>;

    /// Cancel the job of the space `id` before it stops, for `why`.
    fn cancel_job_before_stopping<'a>(&'a self, id: &'a SpaceId, why: JobStop)
    -> BoxFuture<'a, ()>;

    /// Cancel the job running in the space `selector` names, for `why`.
    fn cancel_job_in<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        why: JobStop,
    ) -> BoxFuture<'a, Result<Option<String>>>;

    /// End the job of the space `id` for load protection, for `pressure`,
    /// to rerun once readings recover; whether it did.
    fn stop_job_for_overload<'a>(
        &'a self,
        id: &'a SpaceId,
        pressure: &'static str,
    ) -> BoxFuture<'a, Result<bool>>;

    /// Wake the jobs' loops, since the queue admitted one.
    fn wake_jobs(&self);

    /// Whether the jobs of the repository `id` wait for busy permits.
    fn job_permits_busy(&self, id: &RepositoryId) -> bool;

    /// What a run of `job` of the repository `id` costs.
    fn job_cost<'a>(
        &'a self,
        id: &'a RepositoryId,
        job: &'a QueuedJob,
    ) -> BoxFuture<'a, Result<JobCost>>;

    /// Take the waiting `job` of `repository`, queued as `id`, out of the
    /// queue, passing it to another runner; whether it waited.
    fn pass_over_job<'a>(
        &'a self,
        id: &'a QueueEntryId,
        repository: &'a RepositoryId,
        job: &'a QueuedJob,
    ) -> BoxFuture<'a, Result<bool>>;

    /// What the running jobs use.
    fn job_activities(&self) -> BoxFuture<'_, Result<Vec<Activity>>>;
}

/// The leases executions and agents hold.
pub trait Leases: Send + Sync {
    /// Release the leases the execution `id` acquired.
    fn release_execution_leases<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, ()>;

    /// Release the leases the agent of execution `id` acquired.
    fn release_agent_leases<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, ()>;

    /// Release the leases the stopped agents of the workspace `id` held
    /// until their stop ended.
    fn release_ended_leases_of<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>>;

    /// Give the demo of `execution` in the space `id` its leases.
    fn give_demo_its_leases<'a>(
        &'a self,
        execution: &'a ExecutionId,
        id: &'a SpaceId,
        launch: &'a shoal_protocol::demos::Launch,
    ) -> BoxFuture<'a, ()>;

    /// The ports the workspace `id` reserved.
    fn reserved_ports<'a>(&'a self, id: &'a SpaceId)
    -> BoxFuture<'a, Result<Vec<PortReservation>>>;
}

/// The local reviews an execution may queue.
pub trait Reviews: Send + Sync {
    /// The error to answer `refusal` with: one a missing review caused
    /// says how queueing that review went.
    fn queue_unreviewed(&self, refusal: anyhow::Error) -> BoxFuture<'_, anyhow::Error>;
}

/// The conversions that hand a space's agent over to its new place.
pub trait Conversions: Send + Sync {
    /// Stop the agent a conversion of the space `id` waits to hand over,
    /// keeping it down until `shoal resume` when `holds`; whether one was.
    fn stop_converting_agent<'a>(
        &'a self,
        id: &'a SpaceId,
        holds: bool,
    ) -> BoxFuture<'a, Result<bool>>;

    /// Refuse an agent of the workspace `selector` names while it converts
    /// into a chat or a chat converts into it. The caller holds the agents'
    /// admissions.
    fn ensure_takes_agents<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<()>>;

    /// Hand over the agent of the space `id` before its removal.
    fn hand_over_before_removal<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>>;
}

/// The memory agents' work takes.
pub trait Memory: Send + Sync {
    /// Record what the scope of execution `id` used.
    fn record_scope_memory<'a>(
        &'a self,
        id: &'a ExecutionId,
        memory: ScopeMemory,
    ) -> BoxFuture<'a, ()>;

    /// The limit of the agents' work slice, where one applies.
    fn work_limit(&self) -> Option<WorkLimit>;
}

/// The snapshot the agent queue ranks.
pub trait Ranking: Send + Sync {
    /// What ranking reads now.
    fn ranking_snapshot(&self) -> BoxFuture<'_, Result<Snapshot>>;
}

/// Everything the agents need of the daemon.
pub trait Daemon:
    Spaces
    + Lifecycles
    + Operations
    + Repositories
    + Alerts
    + Turns
    + Merges
    + Demos
    + Links
    + Runner
    + Leases
    + Reviews
    + Conversions
    + Memory
    + Ranking
{
}

impl<T> Daemon for T where
    T: Spaces
        + Lifecycles
        + Operations
        + Repositories
        + Alerts
        + Turns
        + Merges
        + Demos
        + Links
        + Runner
        + Leases
        + Reviews
        + Conversions
        + Memory
        + Ranking
{
}

/// The records of other subsystems the agents read and write in their own
/// transactions.
pub trait Records: Send + Sync {
    /// Forget the commands the agent of the space `id` left running, which
    /// no hook reports the end of once it is idle.
    fn end_commands(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()>;

    /// Whether a stop is recorded for the execution `id`.
    fn stop_recorded(&self, db: &Connection, id: &ExecutionId) -> Result<bool>;

    /// Wake the agent of the space `id` if it is asleep: the restores Shoal
    /// queued on its own for it wait only for their turn in the agent queue.
    fn wake_asleep(&self, db: &Connection, id: &SpaceId) -> Result<()>;

    /// Whether the agent of the space `id`, or of a space below it whose
    /// activity it receives while the spaces `running` have their own,
    /// watches an open PR, whose activity starts its next turn.
    fn receives_open_pr_activity(
        &self,
        db: &Connection,
        id: &SpaceId,
        running: &[SpaceId],
    ) -> Result<bool>;

    /// Whether `shoal done` recorded that the work of the space `id` is
    /// finished.
    fn space_done(&self, db: &Connection, id: &SpaceId) -> Result<bool>;

    /// Whether the space `id` is a workspace, which takes its agent's state
    /// reports whether an agent of it runs or not.
    fn space_is_workspace(&self, db: &Connection, id: &SpaceId) -> Result<bool>;

    /// Whether the space `id` is a chat.
    fn space_is_chat(&self, db: &Connection, id: &SpaceId) -> Result<bool>;

    /// The kind and ID of the space `selector` names, while it exists.
    fn selected_space(
        &self,
        db: &Connection,
        selector: &SpaceSelector,
    ) -> Result<Option<(SpaceKind, SpaceId)>>;

    /// The parent of each space that has one.
    fn parent_ids(&self, db: &Connection) -> Result<std::collections::HashMap<SpaceId, SpaceId>>;

    /// Refuse an alert for the agent of the space `id` once the space is
    /// gone or being removed, since its removal decides from the alerts it
    /// has.
    fn ensure_takes_alerts(&self, db: &Connection, id: &SpaceId) -> Result<()>;

    /// The scope token of the agent of the chat `id`.
    fn agent_token(
        &self,
        db: &Connection,
        id: &SpaceId,
    ) -> Result<Option<shoal_protocol::scope_tokens::ScopeToken>>;

    /// Whether the repository `id` is registered.
    fn repository_registered(&self, db: &Connection, id: &RepositoryId) -> Result<bool>;

    /// What the preparation of the workspace `id` waits for, while it waits.
    fn preparation_waits(&self, db: &Connection, id: &SpaceId) -> Result<Option<PreparationWait>>;

    /// Record that the preparation of the workspace `id` waits for its
    /// agent, which stopped.
    fn note_preparation_stopped(&self, db: &Connection, id: &SpaceId) -> Result<()>;

    /// Record that the agent of the space `id` ended, for the issues it was
    /// assigned.
    fn agent_ended(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()>;

    /// Give the user back the requests the space `id` decides, which its
    /// agent leaves.
    fn release_decided_by(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
    ) -> Result<Vec<ApprovalRequest>>;

    /// Record the demo `launch` the execution `id` runs in the space `space`.
    fn record_demo(
        &self,
        tx: &Transaction<'_>,
        id: &ExecutionId,
        space: &SpaceId,
        launch: &shoal_protocol::demos::Launch,
    ) -> Result<()>;

    /// Record that the demo of the execution `id` runs.
    fn mark_demo_run(&self, tx: &Transaction<'_>, id: &ExecutionId) -> Result<()>;

    /// The launch of the demo the execution `id` runs.
    fn demo_launch(
        &self,
        db: &Connection,
        id: &ExecutionId,
    ) -> Result<Option<shoal_protocol::demos::Launch>>;

    /// What the space `id` records that its agent's terminal shows, with
    /// `pushing` telling which operations still push what the user
    /// approved; none once the space is gone.
    fn space_status(
        &self,
        db: &Connection,
        id: &SpaceId,
        pushing: &dyn Fn(&str) -> bool,
    ) -> Result<Option<SpaceStatus>>;
}

/// The removals under way, which the daemon reserves a space for before its
/// final checks and which the transactions that record new work read.
pub trait Removals: Send + Sync {
    /// Refuse new work, such as a launch it queues, for the space `id` while
    /// a removal holds it or it is not ready.
    fn require_taking_work(&self, db: &Connection, id: &SpaceId) -> Result<()>;

    /// Whether a removal holds the space `id`.
    fn being_removed(&self, id: &SpaceId) -> bool;
}

/// What a space records that its agent's terminal shows.
pub struct SpaceStatus {
    /// The space's name.
    pub name: String,
    /// The newest of its threads whose turn is the user's.
    pub thread: Option<Waiting>,
    /// The oldest of its approval requests the user decides.
    pub approval: Option<Waiting>,
    /// Its recorded completion, with its summary.
    pub done: Option<Option<String>>,
    /// Its first ready mark, with the URL of the item it marks.
    pub ready: Option<Option<String>>,
    pub progress: Steps,
}

/// Something of a space that waits for the user.
pub struct Waiting {
    pub number: ItemNumber,
    /// One line saying what it asks.
    pub about: String,
}

/// Where the agent is in its work.
#[derive(Default)]
pub struct Steps {
    /// Its status line.
    pub line: Option<String>,
    pub done: usize,
    pub total: usize,
}
