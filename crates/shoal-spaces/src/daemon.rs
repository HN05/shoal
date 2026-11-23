//! What the spaces need of the rest of the daemon, which implements it: the
//! operations it runs in workers, the forges of their repositories and the
//! issues, PRs and CI runs they link, the agents that work in them and the
//! queue that starts them, the registered repositories and their settings,
//! the machines of the hub, the CI jobs and reviews that run in workspaces,
//! their leases and demos, the coordination between spaces, and, in their
//! own transactions, what other subsystems record of a space.
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_coordination::Coordination;
use shoal_daemon_base::alerts::AlertKind;
use shoal_daemon_base::blockers::Readiness;
use shoal_daemon_base::stops::{StopPolicy, StopRequest};
use shoal_forge::transport::account::Account;
use shoal_forge_types::items::action::Action;
use shoal_forge_types::items::issue::IssueDetails;
use shoal_forge_types::items::link::{LinkKind, Selection};
use shoal_forge_types::items::reference::{ItemInput, ItemRef};
use shoal_forge_types::review::status::PrStatus;
use shoal_forge_types::{ForgeRepo, PullRequest};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::member_ids::MemberId;
use shoal_ids::move_tickets::MoveTicket;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::agent::Agent;
use shoal_protocol::approvals::ApprovalRequest;
use shoal_protocol::cleanup::Emptying;
use shoal_protocol::containers::ContainerProject;
use shoal_protocol::executions::{ProxyRetry, TurnRetry};
use shoal_protocol::hub::directory::{HubView, Listed};
use shoal_protocol::hub::packages::KeptPackage;
use shoal_protocol::hub::placement::Decision;
use shoal_protocol::hub::{Machine, Member};
use shoal_protocol::links::{Unlinked, issue, pr};
use shoal_protocol::model::{
    Chat, CheckoutSync, Execution, LinkedItem, MergePath, MergeWarning, QueueAdmission, QueueEntry,
    QueuedJob, Relation, Repository, ReviewMark, RunningAgent, Selected, Space, SpaceLinks,
    SpaceRef, StoppedSpace, SyncedRepository, Workspace, WorkspaceRef,
};
use shoal_protocol::processes::Identity;
use shoal_protocol::progress::Blocker;
use shoal_protocol::protocol::operations::{Ended, LaunchRequest, OperationSpec, QueuedLaunch};
use shoal_protocol::protocol::{
    Body, ConfigTarget, ItemContext, Operation, QueuedLaunches, RepoTarget,
};
use shoal_protocol::resources::ResourceLease;
use shoal_protocol::reviews::CarriedReview;
use shoal_protocol::sessions::Origin;
use shoal_protocol::sim::Simulator;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_protocol::state::BlockerKind;
use shoal_protocol::status::IssueStatus;
use shoal_refs::{BranchName, CommitId};
use shoal_schedule::placement::snapshot::Request;
use tokio::sync::{MutexGuard, OwnedMutexGuard};

use crate::Spaces;
use crate::config::repo::ConfigLayers;
use crate::config::{Config, Effective};
use crate::model::EventCause;
use crate::repository::Selector;
use crate::scope::Caller;
use crate::target_checks::TargetCheck;
use shoal_core::launches::SpaceRepository;

/// The operations the daemon runs in workers of their own, and how it
/// answers a request that failed.
pub trait Operations: Send + Sync {
    /// Start `spec` in a worker for `caller`, returning the operation's ID.
    fn start_operation<'a>(
        &'a self,
        spec: OperationSpec,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<String>>;

    /// Start `spec`, which works in the space `workspace`, in a worker as
    /// [`Self::start_operation`] does, returning the operation's ID.
    fn spawn_operation<'a>(
        &'a self,
        workspace: SpaceId,
        spec: OperationSpec,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<String>>;

    /// Whether an operation in the workspace `id` still runs.
    fn operation_runs_in(&self, id: &SpaceId) -> bool;

    /// Whether the operation `id` is over: it ended, or is no longer
    /// followed, having ended long before.
    fn operation_over<'a>(&'a self, id: &'a str) -> BoxFuture<'a, bool>;

    /// How the operation `id` ended, or how its worker reported it ends,
    /// once either came; `None` while the daemon does not follow it.
    fn operation_ending<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Option<Option<Ended>>>;

    /// How the operation `id` ends, once it does; none when it is not
    /// followed, or its record goes first.
    fn operation_ended<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Option<Ended>>;

    /// The check of the space `operation` names, as its kind declares it;
    /// none for a kind that never names one.
    fn target_check(&self, operation: &Operation) -> Option<TargetCheck>;

    /// Whether this daemon starts fake workers that tests drive, as a test
    /// daemon does, instead of running Shoal as them.
    fn fakes_workers(&self) -> bool;

    /// The error body of a request that failed with `error`.
    fn failure(&self, error: &anyhow::Error) -> Body;
}

/// The forges of the spaces' repositories, and the issues, PRs and CI runs
/// spaces link on them.
pub trait Links: Send + Sync {
    /// The repository `remote` names, on the forge its host serves.
    fn forge<'a>(&'a self, remote: &'a str) -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The forge the repository of `item` is on.
    fn item_forge<'a>(&'a self, item: &'a ItemRef) -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The workspace's HEAD: its branch's commit, checking it is still on
    /// the branch, or for a workspace at a commit its detached HEAD,
    /// checking it is still detached.
    fn current_head<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<CommitId>>;

    /// The repository `space` acts in: its own, or the registered one a
    /// chat without one acts in.
    fn acting_repository<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<RepositoryId>>;

    /// Where the forge of `space` is read: the checkout of the repository a
    /// chat without one acts in, the checkout a workspace whose worktree is
    /// still to be created reads in its place, or else where it works.
    fn origin_path<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<PathBuf>>;

    /// The title of the PR `input` names in the repository whose origin
    /// `checkout` has, read with the user's logins.
    fn read_pull_title<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<String>>;

    /// The warning for merging the HEAD of the workspace `selector` names
    /// into the default branch, as its last conflict check found it.
    fn branch_merge_warning<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<MergeWarning>>>;

    /// The forge of the repository `space` acts in.
    fn space_forge<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The forge of the registered checkout's `origin`.
    fn checkout_forge<'a>(&'a self, repository: &'a Repository)
    -> BoxFuture<'a, Result<ForgeRepo>>;

    /// The state of the PRs `pr_cleanup` watches and of the assigned
    /// `issues`, looked up on the forge of `remote`, the space's origin
    /// remote.
    fn linked_states<'a>(
        &'a self,
        remote: &'a Result<Option<String>>,
        pr_cleanup: Option<&'a pr::Registration>,
        issues: &'a [issue::Registration],
    ) -> BoxFuture<'a, (Vec<PrStatus>, Vec<IssueStatus>)>;

    /// The settings of `space` in the repository it acts in.
    fn acting_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>>;

    /// The agent account: the hosts' agent tokens, and the user's own
    /// logins for hosts without one.
    fn agent_account(&self) -> Account;

    /// The user's own logins, which reads and the user's decisions use.
    fn user_account(&self) -> Account;

    /// Settle the agent account on the issues of the space `id` once it
    /// can be.
    fn settle_agents(&self, id: &SpaceId);

    /// Apply `action` as `account` to the item `number` of `kind`, at
    /// `url`, on `forge`, for the space `id`, recording it as the space's
    /// own change.
    fn apply_own<'a>(
        &'a self,
        id: &'a SpaceId,
        forge: &'a ForgeRepo,
        account: &'a Account,
        item: (shoal_forge_types::items::link::ItemKind, u64, String),
        action: &'a Action,
    ) -> BoxFuture<'a, Result<()>>;

    /// Add to each mark what its item leaves out of the repository's
    /// conventions, read as the workspace's agent account. Its description
    /// was checked when Shoal opened it, so only its labels are.
    fn check_conventions<'a>(
        &'a self,
        space: &'a Space,
        settings: &'a Effective,
        marks: &'a mut [ReviewMark],
    ) -> BoxFuture<'a, ()>;

    /// The explicitly selected item, linked or not, of the repository whose
    /// origin `checkout` has, or of a CI run of any.
    fn explicit_item<'a>(
        &'a self,
        checkout: &'a Path,
        selection: &'a Selection,
    ) -> BoxFuture<'a, Result<Option<ItemRef>>>;

    /// Look up the runs of the commit `pushed` left, as the read that sees
    /// a PR merge does, where `[pr] watch_merge_runs` is on.
    fn follow_pushed_merge(&self, pushed: PushedMerge) -> BoxFuture<'_, Result<()>>;

    /// Hand the assignees the space `id` stands for to the machine it moved
    /// to: forget them for it, and Shoal's assignments no other space here
    /// links, so ending it here takes none off the forge.
    fn hand_over_assignees<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>>;

    /// The open issues the forge records as blocking the issue `item` names
    /// in the repository of `context`, by number or URL.
    fn issue_blockers<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<Blocker>>>;

    /// The open issue `item` names in the repository of `context`, by
    /// number or URL, or with `closed` one in any state.
    fn issue_details<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        closed: bool,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<IssueDetails>>;

    /// The forge of the repository whose origin `checkout` has, with the
    /// number and canonical URL of the issue `input` names there.
    fn issue_forge<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<(ForgeRepo, ItemRef)>>;

    /// The space's assigned issues in the order they were assigned.
    fn issue_registrations<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<issue::Registration>>>;

    /// Hold the PR gates of the space `id`, of every space below it and of
    /// `others`, from before a change the space makes through Shoal.
    fn lock_change_gates<'a>(
        &'a self,
        id: &'a SpaceId,
        others: &'a [&'a SpaceId],
    ) -> BoxFuture<'a, Result<shoal_daemon_base::gates::PrGuard>>;

    /// Whether the space `id` looks for the runs of a merge or follows one
    /// it linked that has not finished.
    fn awaits_merge_runs<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// Whether a PR whose merges the space `id` follows merged on the forge
    /// while no read of Shoal's own has seen it.
    fn merged_unseen<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// How the space `selector` names merges its work, from the state the
    /// forge reports for each PR it links as assigned or opened.
    fn merge_path<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<MergePath>>;

    /// The PR registration of the space `id`, refusing an unreadable one.
    fn pr_registration<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<pr::Registration>>>;

    /// The PR `item` names in the repository of `context`, with where its
    /// head is.
    fn pull_request<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<PullRequest>>;

    /// The canonical URL, title and labels of the open issue `input` names
    /// in the repository whose origin `checkout` has, read with the user's
    /// logins. A closed or unknown issue is refused.
    fn read_open_issue<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<OpenIssue>>;

    /// Take the user's logins `dropped` names off their issues where Shoal
    /// added them and no space links the issue for the same login any
    /// longer. Returns why each one that stays did.
    fn release_assignees(&self, dropped: Vec<Dropped>) -> BoxFuture<'_, Vec<String>>;

    /// Link `links` to the space `id` again, as `caller` assigns them.
    /// Each failure becomes a warning.
    fn relink_space<'a>(
        &'a self,
        id: &'a SpaceId,
        links: &'a SpaceLinks,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Vec<String>>;

    /// Stack `stacked` past the base `restack` names: retarget its PRs that
    /// still target a merged head, record its new base and queue a
    /// `base_merged` update for its next watch.
    fn restack<'a>(
        &'a self,
        stacked: &'a WorkspaceRef,
        restack: &'a Restack<'_>,
    ) -> BoxFuture<'a, Result<()>>;

    /// The items `selection` names in the `target` repository; with
    /// `elsewhere`, an issue, PR or CI run URL of another repository selects
    /// that item.
    fn select_context_items<'a>(
        &'a self,
        target: &'a RepoTarget,
        selection: &'a Selection,
        elsewhere: bool,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Selected>>;

    /// The explicitly selected item, or the space's linked items of the
    /// selected kind, with every linked run.
    fn selected_items<'a>(
        &'a self,
        space: &'a Space,
        selection: &'a Selection,
    ) -> BoxFuture<'a, Result<Vec<ItemRef>>>;

    /// Move the space's open PRs at `urls` into or out of draft as its
    /// agent account. The caller holds the gates
    /// [`Self::lock_change_gates`] takes for the space.
    fn set_draft<'a>(
        &'a self,
        space: &'a Space,
        urls: &'a [ItemRef],
        draft: bool,
    ) -> BoxFuture<'a, Result<()>>;

    /// Link the issue `input` names to the space `selector` names as
    /// `caller` assigns it, an agent for a scoped one and else the user,
    /// assigning it on the forge where no assignee is recorded for it;
    /// returns the warnings that gives.
    fn set_issue<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        input: &'a ItemInput,
        title: Option<String>,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<String>>>;

    /// Change the PR registration of the space `selector` names under its
    /// PR gate.
    fn set_pr<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        action: pr::Action,
    ) -> BoxFuture<'a, Result<()>>;

    /// Settle the agent account on `issues`, which the removed `space`
    /// linked as assigned. Returns why each one that could not be settled
    /// was not.
    fn settle_removed_agents<'a>(
        &'a self,
        space: &'a Space,
        issues: &'a [ItemRef],
    ) -> BoxFuture<'a, Result<Vec<String>>>;

    /// Unlink the relation's items of `kind`, or only the one `input` names;
    /// one must be linked.
    fn unlink_items<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        relation: Relation,
        kind: Option<LinkKind>,
        input: Option<&'a ItemInput>,
    ) -> BoxFuture<'a, Result<Unlinked>>;
}

/// The agents that work in spaces and the queue that starts them.
pub trait Agents: Send + Sync {
    /// Whether the agent queue holds back every start, as for a shutdown or
    /// a daemon stop.
    fn agents_suspended(&self) -> bool;

    /// Whether the blockers of the launch waiting to open the workspace `id`
    /// let it start; none without one.
    fn opening_readiness<'a>(&'a self, id: &'a SpaceId)
    -> BoxFuture<'a, Result<Option<Readiness>>>;

    /// Close the agent sessions and places whose spaces are gone.
    fn cleanup_orphan_sessions(&self) -> BoxFuture<'_, ()>;

    /// Queue `launch` as the agent of `space`, waiting for its preparation
    /// when it awaits one, linking `pr` as it starts once `blockers` are
    /// done.
    fn queue_start_agent<'a>(
        &'a self,
        place: (&'a Space, bool),
        launch: QueuedLaunch,
        follows: (Option<ItemInput>, Vec<Blocker>),
    ) -> BoxFuture<'a, Result<QueueAdmission>>;

    /// Give the queue entry `id` the space `space` names.
    fn queue_attach<'a>(
        &'a self,
        id: &'a QueueEntryId,
        space: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<()>>;

    /// Whether disk space holds back new starts.
    fn disk_holds_back_starts(&self) -> BoxFuture<'_, Result<bool>>;

    /// Hold off agent admissions, as while a workspace's conversion into a
    /// chat is recorded.
    fn lock_agent_admissions(&self) -> BoxFuture<'_, MutexGuard<'_, ()>>;

    /// Hold the executions' connections, as registering or stopping one
    /// does.
    fn lock_connections(&self) -> BoxFuture<'_, Box<dyn Send + '_>>;

    /// Whether the execution `id` is connected.
    fn execution_connected<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, bool>;

    /// Forget what is tracked of the executions of the workspace `id`,
    /// which is gone.
    fn forget_workspace_executions<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()>;

    /// The turn the agent of `space` is in, by the execution `execution`
    /// when it names one: its execution and agent.
    fn agent_turn<'a>(
        &'a self,
        space: &'a Space,
        execution: Option<&'a ExecutionId>,
    ) -> BoxFuture<'a, Result<(ExecutionId, Agent)>>;

    /// What was kept of the agents' environment for the repository with
    /// `id`, which may be removed already.
    fn agent_environments<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Vec<BTreeMap<String, String>>>>;

    /// The wrappers of the executions that run agents, each of which owns
    /// its agent wherever the process that started it runs.
    fn agent_wrappers(&self) -> BoxFuture<'_, Result<Vec<Identity>>>;

    /// Mark each of `executions` whose agent protection stopped while its
    /// wrapper waits to restore it.
    fn annotate_stops<'a>(&'a self, executions: &'a mut [Execution]) -> BoxFuture<'a, ()>;

    /// The agent a request from `caller` comes from, and the space and
    /// execution it runs in.
    fn caller_agent<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Option<CallerAgent>>;

    /// Cancel the restores of `workspace_id`'s stopped agents, which stay
    /// stopped until `shoal resume`.
    fn cancel_stopped_restores<'a>(
        &'a self,
        workspace_id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<()>>;

    /// The environment a process that joins the agent of `chat` gets.
    fn chat_environment<'a>(
        &'a self,
        chat: Chat,
    ) -> BoxFuture<'a, Result<BTreeMap<String, OsString>>>;

    /// Refuse `blockers` when they name work of `repository` that a queued
    /// launch could not wait for.
    fn check_blockers<'a>(
        &'a self,
        repository: &'a Selector,
        blockers: Vec<Blocker>,
    ) -> BoxFuture<'a, Result<()>>;

    /// What cleared the space or issue blocker `target` of a space in
    /// `repository`, as admission decides for a launch; none while it
    /// blocks, or for the other kinds, which only the forge clears.
    fn cleared_locally<'a>(
        &'a self,
        repository: Option<&'a RepositoryId>,
        kind: BlockerKind,
        target: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>>>;

    /// End the places of the space `id`, named `name`, which every removal
    /// runs once it committed.
    fn close_space_places<'a>(&'a self, id: &'a SpaceId, name: &'a SpaceName) -> BoxFuture<'a, ()>;

    /// Refuse the scoped `caller` a launch that `replaces` the stopped
    /// agent of the workspace `selector` names, unless it may discard that
    /// agent.
    fn ensure_may_replace<'a>(
        &'a self,
        caller: &'a Caller,
        selector: &'a mut SpaceSelector,
        replaces: bool,
    ) -> BoxFuture<'a, Result<()>>;

    /// Refuse a launch in `space` that replaces its stopped agent while an
    /// agent of its own runs there or waits in the agent queue.
    fn ensure_replaceable<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>>;

    /// Refuse a launch in `space` while it has an agent of its own. Requires
    /// the queue gate, which keeps another launch out until this one is
    /// recorded.
    fn ensure_takes_worker<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>>;

    /// Delete an execution record whose processes the caller proved stopped.
    fn forget_execution<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Result<()>>;

    /// Forget the last sessions of the space, which a new agent or a
    /// finished assignment replaces.
    fn forget_last_sessions(&self, workspace_id: &SpaceId);

    /// Forget the order and times of the state reports of the space `id`,
    /// which is gone.
    fn forget_reports(&self, id: &SpaceId);

    /// Whether an agent launch in the space waits in the queue or starts.
    fn has_queued_launch<'a>(&'a self, space_id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// The executions of the workspace whose agent, stopped between turns,
    /// waits idle to be restored.
    fn idle_restores<'a>(
        &'a self,
        workspace_id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<HashSet<ExecutionId>>>;

    /// Record that the user interacted with the agent of the space
    /// `selector` names now.
    fn note_interaction<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, ()>;

    /// Record that the user interacted with the agent of the space
    /// `selector` names, when `caller` is the user.
    fn record_user_interaction<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, ()>;

    /// The places Shoal created for `space`, or for every space, with the
    /// space each belongs to.
    fn owned_places(&self, space: Option<SpaceId>)
    -> BoxFuture<'_, Result<Vec<(SpaceId, Origin)>>>;

    /// The retry the proxy of one of `executions`, the space's running
    /// ones, waits to make.
    fn proxy_retry(&self, executions: &[Execution]) -> Option<ProxyRetry>;

    /// Queue the worker of the ready `workspace` that `launch` asks for,
    /// once `blockers` are done and it can run there, linking `pr` as it
    /// starts.
    fn queue_agent<'a>(
        &'a self,
        workspace: &'a SpaceSelector,
        launch: LaunchRequest,
        blockers: Vec<Blocker>,
        pr: Option<ItemInput>,
    ) -> BoxFuture<'a, Result<QueueEntry>>;

    /// Queue `message` for the agent of the space `id`, which waits for
    /// its next turn or update instead of starting one.
    fn queue_agent_message_next_turn<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>>;

    /// Every agent that runs or may run, by the execution it ran as.
    fn running_agents(&self) -> BoxFuture<'_, Result<Vec<RunningAgent>>>;

    /// The executions whose stopped agents the existing space `selector`
    /// names saved for `shoal resume`.
    fn saved_executions<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Vec<ExecutionId>>>;

    /// Queue the restore of the stopped `execution` of the space
    /// `workspace` names and start it at once, regardless of the limits.
    fn start_stopped_restore<'a>(
        &'a self,
        workspace: &'a SpaceSelector,
        execution: &'a ExecutionId,
        env: BTreeMap<String, String>,
    ) -> BoxFuture<'a, Result<QueueEntry>>;

    /// Ask the connected wrappers of the space `id` to stop and signal the
    /// survivors of disconnected ones.
    fn stop_executions<'a>(
        &'a self,
        id: &'a SpaceId,
        policy: StopPolicy,
        request: &'a StopRequest,
    ) -> BoxFuture<'a, Result<()>>;

    /// Stop every execution of the space `selector` names, cancelling its
    /// agents' waiting restores.
    fn stop_space<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        request: StopRequest,
    ) -> BoxFuture<'a, Result<()>>;

    /// Stop the agent executions of the space `id`.
    fn stop_space_agent_executions<'a>(
        &'a self,
        id: &'a SpaceId,
        request: &'a StopRequest,
    ) -> BoxFuture<'a, Result<()>>;

    /// Stop the space `selector` names as `shoal stop` does, keeping the
    /// stopped agents as last sessions, and cancel its waiting launches as
    /// `launches` says.
    fn stop_space_manually<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        acknowledge_unproven: bool,
        launches: QueuedLaunches,
    ) -> BoxFuture<'a, Result<StoppedSpace>>;

    /// Record `place`, which the started chat `chat` runs in, as the chat's.
    fn take_chat_place<'a>(&'a self, chat: &'a SpaceId, place: Origin)
    -> BoxFuture<'a, Result<()>>;

    /// Whether the agent of the space `id` ended its turn: it reported
    /// itself idle, or no agent `runs` matches runs any longer.
    fn turn_ended<'a>(
        &'a self,
        id: &'a SpaceId,
        runs: &'a (dyn Fn(&RunningAgent) -> bool + Send + Sync),
    ) -> BoxFuture<'a, Result<bool>>;
}

/// The registered repositories and the settings of the spaces in them.
pub trait Repositories: Send + Sync {
    /// Start a new worktree from its repository's seed snapshot when the
    /// worktree's own settings describe the build that made it.
    fn apply_seed<'a>(
        &'a self,
        repo: &'a Repository,
        workspace: &'a Workspace,
        settings: &'a Effective,
    ) -> BoxFuture<'a, ()>;

    /// How the background sync of the repository went, once it was tried.
    fn checkout_sync(&self, repository_id: &RepositoryId) -> Option<CheckoutSync>;

    /// Destroy the subvolumes among the seed outputs `workspace`'s settings
    /// name, so removing the worktree need not delete their files one by
    /// one.
    fn destroy_seed_outputs<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, ()>;

    /// Refuse the repository `id` while a removal or move of it is under
    /// way or unfinished.
    fn ensure_available<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<()>>;

    /// Fetch the default branch's remotes now, for a decision that needs
    /// what they have.
    fn fetch_default_branch<'a>(
        &'a self,
        repository_id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<()>>;

    /// Whether the default branch of the repository `id` was fetched
    /// recently enough for a decision that needs what its remotes have,
    /// fetching it now otherwise.
    fn default_branch_fresh<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, bool>;

    /// Empty the caches of `repo`, or with `dry_run` say what that would
    /// empty; none without any.
    fn empty_caches<'a>(
        &'a self,
        repo: &'a Repository,
        dry_run: bool,
    ) -> BoxFuture<'a, Result<Option<Emptying>>>;

    /// Forget the rejected config file of the workspace `id`, which is gone.
    fn forget_rejected_config(&self, id: &SpaceId);

    /// The repository `selector` names with its Git gate held, once it is
    /// available.
    fn lock_repository(
        &self,
        selector: Selector,
    ) -> BoxFuture<'_, Result<(Repository, OwnedMutexGuard<()>)>>;

    /// The worktree of the seed of `repo`, under its workspace directory
    /// `root`, once it verifies as Shoal's.
    fn verified_seed_worktree<'a>(
        &'a self,
        repo: &'a Repository,
        root: &'a Path,
    ) -> BoxFuture<'a, Option<PathBuf>>;

    /// The configured `root_dir`, where a chat without a repository runs,
    /// prepared as registering a repository prepares it.
    fn prepare_chat_root(&self) -> BoxFuture<'_, Result<PathBuf>>;

    /// Register a local checkout in place, or clone a URL.
    fn register_repository(
        &self,
        source: Selector,
        name: Option<String>,
        clone_path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<Repository>>;

    /// The repository's settings after every layer.
    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>>;

    /// [`Self::repository_settings`] with the mode and rule files beside the
    /// global config.
    fn repository_settings_with_templates<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// The settings of `space` after every layer.
    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>>;

    /// [`Self::space_settings`] with the mode and rule files beside the
    /// global config, in `repository` for a space without one of its own.
    fn space_settings_with_templates<'a>(
        &'a self,
        space: &'a Space,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// Fetch the repository's default branch and fast-forward the local one,
    /// as the background syncs do.
    fn sync_repository<'a>(
        &'a self,
        selector: &'a Selector,
    ) -> BoxFuture<'a, Result<SyncedRepository>>;

    /// The settings of `target` with the mode and rule files beside the
    /// global config.
    fn target_settings_with_templates(
        &self,
        target: ConfigTarget,
    ) -> BoxFuture<'_, Result<Effective>>;

    /// The config file the checkout of `repository` tracks, as TOML.
    fn tracked_config<'a>(&'a self, repository: &'a Repository) -> BoxFuture<'a, Result<String>>;

    /// The settings of a repository this machine has none of, whose
    /// `tracked` config another machine sent, over the global config.
    fn tracked_settings(&self, tracked: &str) -> Result<Effective>;

    /// The layers of `workspace`, without a worktree file the current format
    /// rejects.
    fn workspace_layers<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<ConfigLayers>>;

    /// The workspace's settings after every layer.
    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// [`Self::workspace_settings`] over a global snapshot the caller also
    /// reads.
    fn workspace_settings_from<'a>(
        &'a self,
        config: &'a Config,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// [`Self::workspace_settings`] with the mode and rule files beside the
    /// global config.
    fn workspace_settings_with_templates<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;
}

/// The machines of the hub this machine is in.
pub trait Hub: Send + Sync {
    /// The answer `machine` gives `operation` from `caller`, which this
    /// machine forwards there through the hub.
    fn ask_machine<'a>(
        &'a self,
        machine: &'a Member,
        operation: Operation,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Body>>;

    /// Send `file` to `machine` with `operation`, from `caller`, and the
    /// answer it gives once it took the file.
    fn deliver_file<'a>(
        &'a self,
        machine: &'a Member,
        operation: Operation,
        caller: Option<&'a Caller>,
        file: &'a Path,
    ) -> BoxFuture<'a, Result<Body>>;

    /// Claim the space `id` at the hub for the move `ticket` through it,
    /// which this machine imports.
    fn claim_moved<'a>(
        &'a self,
        id: &'a SpaceId,
        ticket: &'a MoveTicket,
    ) -> BoxFuture<'a, Result<(), ClaimFailure>>;

    /// Claim the space `id` at the hub, which this machine imports as
    /// another moves it here.
    fn claim_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()>;

    /// Claim the space `id` at the hub for this machine's takeover of it
    /// from the package the hub keeps of it.
    fn claim_taken_over<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<(), ClaimFailure>>;

    /// Whether a space above the space `id` on another machine has an
    /// agent, which may work in it.
    fn coordinator_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, bool>;

    /// The spaces below the space `id` on other machines, deepest first.
    fn descendants_elsewhere<'a>(&'a self, id: &'a SpaceId)
    -> BoxFuture<'a, Result<Vec<SpaceRef>>>;

    /// Refuse the space `parent`, which runs on another machine, as the
    /// parent of the space `space` of `repository`.
    fn ensure_parent_elsewhere<'a>(
        &'a self,
        space: Option<&'a SpaceId>,
        repository: Option<&'a RepositoryId>,
        parent: &'a SpaceId,
    ) -> BoxFuture<'a, Result<()>>;

    /// Write the package the hub keeps of the space `id` to the new file
    /// `file`.
    fn fetch_package<'a>(&'a self, id: &'a SpaceId, file: &'a Path) -> BoxFuture<'a, Result<()>>;

    /// The other machines' spaces `caller` sees.
    fn hub_view<'a>(&'a self, caller: Option<&'a Caller>)
    -> BoxFuture<'a, Result<Option<HubView>>>;

    /// The package the hub keeps of the space `id`, whose machine is
    /// offline.
    fn kept_package<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<KeptPackage>>>;

    /// Note that the space `id` is listed here now.
    fn listed<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()>;

    /// The name this machine goes by in its hub, else its host name.
    fn machine_name(&self) -> BoxFuture<'_, String>;

    /// The other joined machine whose space `selector` names, as the hub's
    /// directory lists it.
    fn machine_of<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<(Member, Listed)>>>;

    /// The joined machine `named` names, by its ID or a name only one
    /// machine has, and whether it is this machine.
    fn named_machine<'a>(&'a self, named: &'a str) -> BoxFuture<'a, Result<(Member, bool)>>;

    /// The refusal of files attached here on what goes to `elsewhere`,
    /// which names where it runs, as "machine desk".
    fn files_stay_here(&self, elsewhere: &str) -> anyhow::Error;

    /// Whether `error` is of a call through the hub that never left this
    /// side, which the other side never ran.
    fn never_sent(&self, error: &anyhow::Error) -> bool;

    /// The URL the origin of the repository of the space `id`, which
    /// another machine runs, is cloned from.
    fn origin_url_elsewhere<'a>(&'a self, id: &'a SpaceId)
    -> BoxFuture<'a, Result<Option<String>>>;

    /// The space above the space `id` when it runs on another machine.
    fn parent_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<SpaceRef>>>;

    /// Where a start of `request` goes among the hub's machines.
    fn place_start(&self, request: Request) -> BoxFuture<'_, Result<Option<Decision>>>;

    /// Refuse to open the space `id` again while the hub lists it on
    /// another machine.
    fn refuse_live_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>>;

    /// Remove the spaces directly below the space `id` that other machines
    /// run as `below` asks, returning their names.
    fn remove_children_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
        below: BelowElsewhere<'a>,
    ) -> BoxFuture<'a, Result<Vec<SpaceName>>>;

    /// The repository of the space `id`, which another machine runs.
    fn repository_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<SpaceRepository>>;

    /// Settle this machine's move `ticket` of the space `id` to `machine`:
    /// whether that machine claimed the space.
    fn settle_move<'a>(
        &'a self,
        id: &'a SpaceId,
        machine: &'a MemberId,
        ticket: &'a MoveTicket,
    ) -> BoxFuture<'a, Result<bool>>;

    /// This machine as the hub names it.
    fn this_machine(&self) -> BoxFuture<'_, Result<Option<Machine>>>;
}

/// The CI jobs this machine's runner runs in workspaces.
pub trait Runner: Send + Sync {
    /// Name the job each workspace among `spaces` that runs one runs.
    fn attach_jobs<'a>(&'a self, spaces: &'a mut [Space]) -> BoxFuture<'a, Result<()>>;

    /// Cancel the job of the workspace `id` before its runner stops, as
    /// `why` stops it.
    fn cancel_job_before_stopping<'a>(
        &'a self,
        id: &'a SpaceId,
        why: shoal_daemon_base::jobs::JobStop,
    ) -> BoxFuture<'a, ()>;

    /// The workspaces of the jobs running for what the space `id` links.
    fn linked_job_workspaces<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<Workspace>>>;

    /// The entries of the jobs waiting to start for what the space `id`
    /// links.
    fn linked_waiting_jobs<'a>(&'a self, id: &'a SpaceId)
    -> BoxFuture<'a, Result<Vec<QueueEntry>>>;

    /// The job the runner runs in the workspace `id`, if any.
    fn workspace_job<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<Box<QueuedJob>>>>;
}

/// The reviews of workspaces and the gate agent.
pub trait Reviews: Send + Sync {
    /// Refuse `caller`, where the setting for `gate` holds it, unless a
    /// finished review covers `head`.
    fn check_review<'a>(
        &'a self,
        workspace: &'a Workspace,
        gate: ReviewGate,
        head: &'a CommitId,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>>;

    /// [`Self::check_review`], queueing the review it misses.
    fn require_review<'a>(
        &'a self,
        workspace: &'a Workspace,
        gate: ReviewGate,
        head: &'a CommitId,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>>;

    /// `error`, with a review of the workspace queued where it is a refusal
    /// for want of one, which then says how that went.
    fn queue_unreviewed(&self, error: anyhow::Error) -> BoxFuture<'_, anyhow::Error>;

    /// Refuse `request` without asking the gate when the gate decides it and
    /// has denied the same action too often.
    fn ensure_gate_hears<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>>;

    /// The queued or running review of the workspace `id`, as a package
    /// carries it to the machine that imports the workspace.
    fn pending_review<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<CarriedReview>>>;

    /// Queue again on this machine the review the package of the space `id`
    /// carried.
    fn queue_carried_review<'a>(
        &'a self,
        id: &'a SpaceId,
        review: CarriedReview,
    ) -> BoxFuture<'a, Result<()>>;

    /// Stop the workspace's running review and the answers made for it.
    fn stop_reviews<'a>(&'a self, workspace_id: &'a SpaceId) -> BoxFuture<'a, ()>;
}

/// The ports, simulators, containers and resource leases spaces hold.
pub trait Leases: Send + Sync {
    /// The container project of the workspace `id`.
    fn container_project<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<ContainerProject>>>;

    /// The resource leases of the space `selector` names, or of every one.
    fn list_resources<'a>(
        &'a self,
        selector: Option<&'a SpaceSelector>,
    ) -> BoxFuture<'a, Result<Vec<ResourceLease>>>;

    /// The managed simulators of `owner`, or every one.
    fn list_simulators<'a>(
        &'a self,
        owner: Option<&'a SpaceId>,
    ) -> BoxFuture<'a, Result<Vec<Simulator>>>;

    /// Take the container project of `workspace` down with its volumes.
    fn remove_containers<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>>;

    /// Delete the simulators `owner` holds.
    fn remove_simulators<'a>(&'a self, owner: &'a SpaceId) -> BoxFuture<'a, Result<()>>;

    /// Run the release hook `command` of `workspace` for `lease`; a failure
    /// keeps the lease.
    fn run_resource_release_hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        lease: &'a ResourceLease,
        command: Option<&'a Path>,
    ) -> BoxFuture<'a, Result<()>>;
}

/// The demos spaces run.
pub trait Demos: Send + Sync {
    /// Ask the space's demo `name`, or every demo there, to stop.
    fn stop_demos<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        name: Option<String>,
    ) -> BoxFuture<'a, Result<()>>;
}

/// What other subsystems record of a space, which the spaces read and write
/// in their own transactions.
pub trait Records: Send + Sync {
    /// Fill in what other subsystems record of `workspace`: its latest
    /// review, its agent's state, sleep, wake, restart and place in the
    /// queue, and its links.
    fn load_workspace(&self, db: &Connection, workspace: &mut Workspace) -> Result<()>;

    /// Fill in what other subsystems record of `chat`: its agent's state,
    /// sleep, wake, restart and place in the queue, and its links.
    fn load_chat(&self, db: &Connection, chat: &mut Chat) -> Result<()>;

    /// The items of `kind`, or of every kind, the space `id` links as
    /// assigned.
    fn assigned_items(
        &self,
        db: &Connection,
        id: &SpaceId,
        kind: Option<shoal_forge_types::items::link::ItemKind>,
    ) -> Result<Vec<ItemRef>>;

    /// The items the space `id` links in `relation`.
    fn recorded_items(
        &self,
        db: &Connection,
        id: &SpaceId,
        relation: Relation,
    ) -> Result<Vec<LinkedItem>>;

    /// The issues, PRs and runs the space `id` links.
    fn space_links(&self, db: &Connection, id: &SpaceId) -> Result<SpaceLinks>;

    /// The state the agent of the space `id` last reported.
    fn agent_state(
        &self,
        db: &Connection,
        id: &SpaceId,
    ) -> Result<Option<shoal_protocol::state::AgentState>>;

    /// Hand the alerts the space `id`'s agent did not read to the user, as
    /// its removal does.
    /// Returns the alert the user got for them, if any.
    fn hand_alerts_to_user(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<i64>>;

    /// Forget the logins the removed space `id` stood for on its issues,
    /// and its issue links, returning them.
    fn remove_issue_links(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
    ) -> Result<(Vec<Dropped>, Vec<ItemRef>)>;

    /// Drop the pending reviews the removed space `id` writes, or of it,
    /// with their comments.
    fn drop_pending_reviews(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()>;

    /// Cancel the wake timer and the turn retry of the space `id`, whose
    /// work finished.
    fn cancel_wake(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()>;

    /// The retry of the failed turn of the agent of the space `id`.
    fn turn_retry(&self, db: &Connection, id: &SpaceId) -> Result<Option<TurnRetry>>;

    /// The executions, with their spaces, other than `own`, of the space
    /// `space` or of every space.
    fn other_executions(
        &self,
        db: &Connection,
        space: Option<&SpaceId>,
        own: Option<&ExecutionId>,
    ) -> Result<Vec<(SpaceId, ExecutionId)>>;

    /// The executions of the space `id` that run an agent, other than
    /// those whose restore waits in the queue.
    fn running_agents(&self, db: &Connection, id: &SpaceId) -> Result<Vec<ExecutionId>>;

    /// The agent the newest queue entry of the space `id` starts.
    fn queued_agent(&self, db: &Connection, id: &SpaceId) -> Result<Option<Agent>>;

    /// Whether a queued launch waits to open the workspace `id`.
    fn launch_waits_to_open(&self, db: &Connection, id: &SpaceId) -> Result<bool>;

    /// Forget the executions of the space `id` and the queue entries that
    /// restore or opened them, as its kind changes under it.
    fn forget_executions(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()>;

    /// Forget the executions of the space `id` that run no agent.
    fn forget_command_executions(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()>;

    /// Mark what other subsystems recorded as running when the daemon
    /// stopped as interrupted: executions and simulator clean requests.
    fn interrupt_operations(&self, tx: &Transaction<'_>) -> Result<()>;

    /// Every registered repository.
    fn repositories(&self, db: &Connection) -> Result<Vec<Repository>>;

    /// Record that the repository `id` was used last.
    fn touch_repository(&self, tx: &Transaction<'_>, id: &RepositoryId) -> Result<()>;

    /// The workspaces that hold a lease of the pool or resource `name`.
    fn leasing_workspaces(&self, db: &Connection, name: &str) -> Result<Vec<SpaceId>>;

    /// Take the number the hub's sequence gives the new space `id`, on a
    /// machine that numbers its spaces from a hub's sequence.
    fn next_space_number(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<SpaceNumber>>;

    /// Refuse to take files at `paths` while alerts hold more than fit.
    fn ensure_holders_fit(&self, tx: &Transaction<'_>, paths: &[PathBuf]) -> Result<()>;

    /// What coordination records of the spaces, as it reads and writes them
    /// in its own transactions.
    fn coordination(&self) -> &dyn shoal_coordination::daemon::Records;
}

/// The agent a request comes from, and where it runs.
pub struct CallerAgent {
    pub execution: ExecutionId,
    pub name: Agent,
    pub space_id: SpaceId,
}

/// Why the claim of a space that a move through the hub brings failed.
#[derive(Debug)]
pub enum ClaimFailure {
    /// The hub took no claim.
    Refused(anyhow::Error),
    /// The hub may have taken it, as when its answer never came.
    Unknown(anyhow::Error),
}

/// What a removal asks of the spaces directly below its space that other
/// machines run, each of which its own machine removes as it removes a
/// child of a space it removes.
pub struct BelowElsewhere<'a> {
    /// Leave the spaces the user holds, as a workspace process's removal
    /// does.
    pub refuse_holds: bool,
    /// The IDs of the only spaces below it the removal takes, when the
    /// caller confirmed those it saw.
    pub confirmed: Option<&'a [SpaceId]>,
    /// The cause of the removal this one is part of.
    pub cause: EventCause,
}

/// Where the workspaces stacked on a base go once its work merged.
pub struct Restack<'a> {
    pub base: &'a Workspace,
    /// The base's HEAD, past which stacked branches rebase.
    pub head: &'a CommitId,
    /// The branches a stacked PR may still target, which it retargets.
    pub merged_heads: &'a [BranchName],
    /// The branch the base's work merged into, which stacked ones rebase
    /// onto and their PRs target.
    pub target: &'a BranchName,
    pub onto: Onto,
    /// The workspace stacked ones build on next; none for the default
    /// branch.
    pub new_base: Option<SpaceId>,
    pub cause: EventCause,
    /// The account retargeting goes out as.
    pub account: &'a Account,
}

/// Where the branch the base merged into is for a stacked branch to rebase
/// onto.
#[derive(Clone, Copy)]
pub enum Onto {
    /// On the remote, fetched first.
    Remote,
    /// A local branch of the repository.
    Local,
    /// The stacked branch has the base's work already, so it needs no
    /// rebase.
    Held,
}

/// A merge of a space's branch that `shoal merge` pushed to the default
/// branch's upstream.
pub struct PushedMerge {
    pub space: SpaceId,
    /// The URLs it was pushed to.
    pub remotes: Vec<String>,
    /// The remote's branch that took it.
    pub branch: BranchName,
    /// The commit it left there.
    pub commit: CommitId,
}

/// An open issue a new workspace is assigned.
#[derive(Debug, Clone)]
pub struct OpenIssue {
    pub url: ItemRef,
    pub title: String,
    pub labels: Vec<String>,
}

/// The login a space stood for among an issue's assignees, until it
/// dropped the issue's link.
#[derive(Debug, Clone)]
pub struct Dropped {
    pub url: String,
    pub login: String,
}

/// What a review gate guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewGate {
    /// `shoal ready`.
    Ready,
    /// `shoal merge`.
    Merge,
}

/// Everything the spaces need of the rest of the daemon, which each of
/// their operations that reaches beyond them takes.
pub trait Daemon:
    shoal_coordination::daemon::Daemon
    + Operations
    + Links
    + Agents
    + Repositories
    + Hub
    + Runner
    + Reviews
    + Leases
    + Demos
{
    /// The spaces service.
    fn spaces(&self) -> &Spaces;

    /// The coordination service, whose operations take this daemon.
    fn coordination(&self) -> &Coordination;
}
