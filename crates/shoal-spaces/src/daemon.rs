//! What the spaces need of the rest of the daemon, which implements it.
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
use shoal_forge_types::activity::status::PrStatus;
use shoal_forge_types::ci::run_action::Run;
use shoal_forge_types::items::action::Action;
use shoal_forge_types::items::issue::IssueDetails;
use shoal_forge_types::items::link::{LinkKind, Selection};
use shoal_forge_types::items::reference::{ItemInput, ItemRef};
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
use shoal_protocol::model::KeptLease;
use shoal_protocol::model::{
    Chat, CheckoutSync, Execution, LinkedItem, MergePath, MergeWarning, QueueAdmission, QueueEntry,
    QueuedJob, Relation, Repository, ReviewMark, RunningAgent, Selected, Space, SpaceLinks,
    SpaceRef, StoppedSpace, SyncedRepository, Workspace, WorkspaceRef,
};
use shoal_protocol::processes::Identity;
use shoal_protocol::progress::Blocker;
use shoal_protocol::protocol::operations::{LaunchRequest, QueuedLaunch};
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

/// The operations the daemon runs in workers of their own.
pub trait Operations: Send + Sync {
    fn target_check(&self, operation: &Operation) -> Option<TargetCheck>;

    fn failure(&self, error: &anyhow::Error) -> Body;
}

/// The forges of the spaces' repositories, and the issues, PRs and CI runs
/// spaces link on them.
pub trait Links: Send + Sync {
    /// Checks that the workspace is still on its branch, or still detached
    /// for a workspace at a commit.
    fn current_head<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<CommitId>>;

    /// The space's own repository, or for a chat without one the registered
    /// repository it acts in.
    fn acting_repository<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<RepositoryId>>;

    /// Where the space's forge is read: for a chat without a repository the
    /// checkout it acts in, for a workspace without its worktree yet its
    /// checkout, and else where the space works.
    fn origin_path<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<PathBuf>>;

    /// Reads the PR with the user's logins.
    fn read_pull_title<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<String>>;

    /// The warning the workspace's last conflict check found; no new check
    /// runs.
    fn branch_merge_warning<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<MergeWarning>>>;

    fn space_forge<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<ForgeRepo>>;

    /// `remote` is the space's origin remote.
    fn linked_states<'a>(
        &'a self,
        remote: &'a Result<Option<String>>,
        pr_cleanup: Option<&'a pr::Registration>,
        issues: &'a [issue::Registration],
    ) -> BoxFuture<'a, (Vec<PrStatus>, Vec<IssueStatus>)>;

    fn acting_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>>;

    /// Runs in the background and settles once the account can be.
    fn settle_agents(&self, id: &SpaceId);

    /// Records the action as the space's own change, which its watch leaves
    /// out.
    fn apply_own<'a>(
        &'a self,
        id: &'a SpaceId,
        forge: &'a ForgeRepo,
        account: &'a Account,
        item: (shoal_forge_types::items::link::ItemKind, u64, String),
        action: &'a Action,
    ) -> BoxFuture<'a, Result<()>>;

    /// Reads as the workspace's agent account. Only labels are checked: Shoal
    /// checked the description when it opened the item.
    fn check_conventions<'a>(
        &'a self,
        space: &'a Space,
        settings: &'a Effective,
        marks: &'a mut [ReviewMark],
    ) -> BoxFuture<'a, ()>;

    /// The item may be linked or not, and a CI run may be of any repository.
    fn explicit_item<'a>(
        &'a self,
        checkout: &'a Path,
        selection: &'a Selection,
    ) -> BoxFuture<'a, Result<Option<ItemRef>>>;

    /// Does nothing unless `[pr] watch_merge_runs` is on.
    fn follow_pushed_merge(&self, pushed: PushedMerge) -> BoxFuture<'_, Result<()>>;

    /// Forgets the space's assignees, and Shoal's assignments no other space
    /// here links, so that ending the space here takes none off the forge.
    fn hand_over_assignees<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>>;

    fn issue_blockers<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<Blocker>>>;

    /// Refuses a closed issue unless `closed` is set.
    fn issue_details<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        closed: bool,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<IssueDetails>>;

    fn issue_forge<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<(ForgeRepo, ItemRef)>>;

    /// In the order they were assigned.
    fn issue_registrations<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<issue::Registration>>>;

    /// Locks the gates of the space, every space below it and `others`. Take
    /// them before a change the space makes through Shoal.
    fn lock_change_gates<'a>(
        &'a self,
        id: &'a SpaceId,
        others: &'a [&'a SpaceId],
    ) -> BoxFuture<'a, Result<shoal_daemon_base::gates::PrGuard>>;

    fn awaits_merge_runs<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// Whether a PR whose merges the space follows merged on the forge
    /// before any read of Shoal's saw it.
    fn merged_unseen<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// Decided from the forge state of each PR the space links as assigned
    /// or opened.
    fn merge_path<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<MergePath>>;

    /// Refuses an unreadable registration.
    fn pr_registration<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<pr::Registration>>>;

    fn pull_request<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<PullRequest>>;

    /// `run` is a URL or the number in it, read as `caller` may link it.
    fn launch_run<'a>(
        &'a self,
        context: &'a ItemContext,
        run: &'a ItemInput,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Run>>;

    fn link_related_run<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        run: &'a ItemRef,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Reads the issue with the user's logins and refuses a closed or
    /// unknown one.
    fn read_open_issue<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<OpenIssue>>;

    /// Leaves a login that another space still links the issue for. Returns
    /// why each kept login stays.
    fn release_assignees(&self, dropped: Vec<Dropped>) -> BoxFuture<'_, Vec<String>>;

    /// Each failure becomes a warning.
    fn relink_space<'a>(
        &'a self,
        id: &'a SpaceId,
        links: &'a SpaceLinks,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Vec<String>>;

    /// Retargets the PRs that still target a merged head, records the new
    /// base and queues a `base_merged` update for the next watch.
    fn restack<'a>(
        &'a self,
        stacked: &'a WorkspaceRef,
        restack: &'a Restack<'_>,
    ) -> BoxFuture<'a, Result<()>>;

    /// With `elsewhere`, an issue, PR or CI run URL of another repository
    /// selects that item.
    fn select_context_items<'a>(
        &'a self,
        target: &'a RepoTarget,
        selection: &'a Selection,
        elsewhere: bool,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Selected>>;

    /// The explicitly selected item, or else the space's linked items of the
    /// selected kind, with every linked run.
    fn selected_items<'a>(
        &'a self,
        space: &'a Space,
        selection: &'a Selection,
    ) -> BoxFuture<'a, Result<Vec<ItemRef>>>;

    /// Acts as the space's agent account. The caller holds the gates
    /// [`Self::lock_change_gates`] takes for the space.
    fn set_draft<'a>(
        &'a self,
        space: &'a Space,
        urls: &'a [ItemRef],
        draft: bool,
    ) -> BoxFuture<'a, Result<()>>;

    /// Assigns it on the forge where no assignee is recorded for it. A scoped
    /// `caller` assigns as an agent, else as the user.
    fn set_issue<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        input: &'a ItemInput,
        title: Option<String>,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<String>>>;

    /// Runs under the space's PR gate.
    fn set_pr<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        action: pr::Action,
    ) -> BoxFuture<'a, Result<()>>;

    /// Returns why each issue that could not be settled was not.
    fn settle_removed_agents<'a>(
        &'a self,
        space: &'a Space,
        issues: &'a [ItemRef],
    ) -> BoxFuture<'a, Result<Vec<String>>>;

    /// Refuses when nothing is linked.
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
    /// Whether the queue holds back every start, as for a shutdown or a
    /// daemon stop.
    fn agents_suspended(&self) -> bool;

    /// None when no launch waits to open the workspace.
    fn opening_readiness<'a>(&'a self, id: &'a SpaceId)
    -> BoxFuture<'a, Result<Option<Readiness>>>;

    fn cleanup_orphan_sessions(&self) -> BoxFuture<'_, ()>;

    /// Waits for the space's preparation when it awaits one, and links `pr`
    /// once `blockers` are done and the agent starts.
    fn queue_start_agent<'a>(
        &'a self,
        place: (&'a Space, bool),
        launch: QueuedLaunch,
        follows: (Option<ItemInput>, Vec<Blocker>),
    ) -> BoxFuture<'a, Result<QueueAdmission>>;

    fn queue_attach<'a>(
        &'a self,
        id: &'a QueueEntryId,
        space: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<()>>;

    fn disk_holds_back_starts(&self) -> BoxFuture<'_, Result<bool>>;

    /// Held while a workspace's conversion into a chat is recorded.
    fn lock_agent_admissions(&self) -> BoxFuture<'_, MutexGuard<'_, ()>>;

    /// Held while registering or stopping an execution.
    fn lock_connections(&self) -> BoxFuture<'_, Box<dyn Send + '_>>;

    fn execution_connected<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, bool>;

    fn forget_workspace_executions<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()>;

    /// Looks the turn up by `execution` when it names one.
    fn agent_turn<'a>(
        &'a self,
        space: &'a Space,
        execution: Option<&'a ExecutionId>,
    ) -> BoxFuture<'a, Result<(ExecutionId, Agent)>>;

    /// The repository may be removed already.
    fn agent_environments<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Vec<BTreeMap<String, String>>>>;

    /// Each wrapper owns its agent, wherever the process that started it
    /// runs.
    fn agent_wrappers(&self) -> BoxFuture<'_, Result<Vec<Identity>>>;

    /// Marks each execution whose agent protection stopped and whose wrapper
    /// waits to restore it.
    fn annotate_stops<'a>(&'a self, executions: &'a mut [Execution]) -> BoxFuture<'a, ()>;

    fn caller_agent<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Option<CallerAgent>>;

    /// The stopped agents stay stopped until `shoal resume`.
    fn cancel_stopped_restores<'a>(
        &'a self,
        workspace_id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<()>>;

    fn chat_environment<'a>(
        &'a self,
        chat: Chat,
    ) -> BoxFuture<'a, Result<BTreeMap<String, OsString>>>;

    /// Refuses blockers that name work of `repository` a queued launch could
    /// not wait for.
    fn check_blockers<'a>(
        &'a self,
        repository: &'a Selector,
        blockers: Vec<Blocker>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Decides as admission does for a launch. None while the blocker blocks,
    /// and for the other kinds, which only the forge clears.
    fn cleared_locally<'a>(
        &'a self,
        repository: Option<&'a RepositoryId>,
        kind: BlockerKind,
        target: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>>>;

    /// Every removal runs this once it has committed.
    fn close_space_places<'a>(&'a self, id: &'a SpaceId, name: &'a SpaceName) -> BoxFuture<'a, ()>;

    /// Refuses the scoped `caller` unless it may discard the stopped agent.
    fn ensure_may_replace<'a>(
        &'a self,
        caller: &'a Caller,
        selector: &'a mut SpaceSelector,
        replaces: bool,
    ) -> BoxFuture<'a, Result<()>>;

    /// Refuses while an agent of the space's own runs there or waits in the
    /// agent queue.
    fn ensure_replaceable<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>>;

    /// Discards the agent's last sessions and stopped commands too, and
    /// refuses an agent that runs or waits in the agent queue.
    fn discard_replaced_agent<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>>;

    /// Requires the queue gate, which keeps another launch out until this
    /// one is recorded.
    fn ensure_takes_worker<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>>;

    /// The caller must have proved the execution's processes stopped.
    fn forget_execution<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Result<()>>;

    /// A new agent or a finished assignment replaces them.
    fn forget_last_sessions(&self, workspace_id: &SpaceId);

    fn forget_reports(&self, id: &SpaceId);

    fn has_queued_launch<'a>(&'a self, space_id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// The executions whose agent stopped between turns and waits idle to
    /// be restored.
    fn idle_restores<'a>(
        &'a self,
        workspace_id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<HashSet<ExecutionId>>>;

    fn note_interaction<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, ()>;

    /// Records nothing unless `caller` is the user.
    fn record_user_interaction<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, ()>;

    /// For `space`, or for every space when it is `None`.
    fn owned_places(&self, space: Option<SpaceId>)
    -> BoxFuture<'_, Result<Vec<(SpaceId, Origin)>>>;

    fn proxy_retry(&self, executions: &[Execution]) -> Option<ProxyRetry>;

    /// Starts it once `blockers` are done and it can run there, linking
    /// `pr` as it starts.
    fn queue_agent<'a>(
        &'a self,
        workspace: &'a SpaceSelector,
        launch: LaunchRequest,
        blockers: Vec<Blocker>,
        pr: Option<ItemInput>,
    ) -> BoxFuture<'a, Result<QueueEntry>>;

    /// The message waits for the agent's next turn or update instead of
    /// starting one.
    fn queue_agent_message_next_turn<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>>;

    fn running_agents(&self) -> BoxFuture<'_, Result<Vec<RunningAgent>>>;

    /// The executions whose stopped agents the space saved for `shoal
    /// resume`.
    fn saved_executions<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Vec<ExecutionId>>>;

    /// Starts it at once, regardless of the limits.
    fn start_stopped_restore<'a>(
        &'a self,
        workspace: &'a SpaceSelector,
        execution: &'a ExecutionId,
        env: BTreeMap<String, String>,
    ) -> BoxFuture<'a, Result<QueueEntry>>;

    /// Asks the connected wrappers to stop and signals the survivors of
    /// disconnected ones.
    fn stop_executions<'a>(
        &'a self,
        id: &'a SpaceId,
        policy: StopPolicy,
        request: &'a StopRequest,
    ) -> BoxFuture<'a, Result<()>>;

    /// Cancels its agents' waiting restores too.
    fn stop_space<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        request: StopRequest,
    ) -> BoxFuture<'a, Result<()>>;

    fn stop_space_agent_executions<'a>(
        &'a self,
        id: &'a SpaceId,
        request: &'a StopRequest,
    ) -> BoxFuture<'a, Result<()>>;

    /// Stops as `shoal stop` does: the stopped agents stay as last
    /// sessions, and `launches` says which waiting launches to cancel.
    fn stop_space_manually<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        acknowledge_unproven: bool,
        launches: QueuedLaunches,
    ) -> BoxFuture<'a, Result<StoppedSpace>>;

    fn take_chat_place<'a>(&'a self, chat: &'a SpaceId, place: Origin)
    -> BoxFuture<'a, Result<()>>;

    /// The agent reported itself idle, or no agent `runs` matches runs any
    /// longer.
    fn turn_ended<'a>(
        &'a self,
        id: &'a SpaceId,
        runs: &'a (dyn Fn(&RunningAgent) -> bool + Send + Sync),
    ) -> BoxFuture<'a, Result<bool>>;
}

/// The registered repositories and the settings of the spaces in them.
pub trait Repositories: Send + Sync {
    /// Applies the snapshot only when the worktree's own settings describe
    /// the build that made it.
    fn apply_seed<'a>(
        &'a self,
        repo: &'a Repository,
        workspace: &'a Workspace,
        settings: &'a Effective,
    ) -> BoxFuture<'a, ()>;

    /// None until the sync was tried.
    fn checkout_sync(&self, repository_id: &RepositoryId) -> Option<CheckoutSync>;

    /// Destroys the subvolumes among the seed outputs, so that removing the
    /// worktree need not delete their files one by one.
    fn destroy_seed_outputs<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, ()>;

    /// Refuses while a removal or move of the repository is under way or
    /// unfinished.
    fn ensure_available<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<()>>;

    fn fetch_default_branch<'a>(
        &'a self,
        repository_id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<()>>;

    /// Fetches it now when the last fetch is too old for a decision that
    /// needs what the remotes have.
    fn default_branch_fresh<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, bool>;

    /// With `dry_run`, only says what it would empty.
    fn empty_caches<'a>(
        &'a self,
        repo: &'a Repository,
        dry_run: bool,
    ) -> BoxFuture<'a, Result<Option<Emptying>>>;

    fn forget_rejected_config(&self, id: &SpaceId);

    /// Takes the Git gate, then refuses the repository while a removal or
    /// move of it is under way or unfinished.
    fn lock_repository(
        &self,
        selector: Selector,
    ) -> BoxFuture<'_, Result<(Repository, OwnedMutexGuard<()>)>>;

    /// None unless the worktree under `root` verifies as Shoal's.
    fn verified_seed_worktree<'a>(
        &'a self,
        repo: &'a Repository,
        root: &'a Path,
    ) -> BoxFuture<'a, Option<PathBuf>>;

    /// Prepares `root_dir`, where a chat without a repository runs, as
    /// registering a repository prepares it.
    fn prepare_chat_root(&self) -> BoxFuture<'_, Result<PathBuf>>;

    /// Registers a local checkout in place, or clones a URL.
    fn register_repository(
        &self,
        source: Selector,
        name: Option<String>,
        clone_path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<Repository>>;

    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>>;

    /// Adds the mode and rule files beside the global config.
    fn repository_settings_with_templates<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Effective>>;

    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>>;

    /// Adds the mode and rule files beside the global config. A space without
    /// a repository of its own reads `repository`.
    fn space_settings_with_templates<'a>(
        &'a self,
        space: &'a Space,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// Fast-forwards the local default branch as the background syncs do.
    fn sync_repository<'a>(
        &'a self,
        selector: &'a Selector,
    ) -> BoxFuture<'a, Result<SyncedRepository>>;

    /// Adds the mode and rule files beside the global config.
    fn target_settings_with_templates(
        &self,
        target: ConfigTarget,
    ) -> BoxFuture<'_, Result<Effective>>;

    fn tracked_config<'a>(&'a self, repository: &'a Repository) -> BoxFuture<'a, Result<String>>;

    /// For a repository this machine does not have: `tracked` is the config
    /// another machine sent, applied over the global config.
    fn tracked_settings(&self, tracked: &str) -> Result<Effective>;

    /// Leaves out a worktree file the current format rejects.
    fn workspace_layers<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<ConfigLayers>>;

    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// Reads `global`, which the caller also reads, instead of a new
    /// snapshot.
    fn workspace_settings_from<'a>(
        &'a self,
        config: &'a Config,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// Adds the mode and rule files beside the global config.
    fn workspace_settings_with_templates<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>>;
}

/// The machines of the hub this machine is in. Coordination's interface
/// asks for them too.
pub trait Hub: Send + Sync {
    /// Returns the answer `machine` gives once it took the file.
    fn deliver_file<'a>(
        &'a self,
        machine: &'a Member,
        operation: Operation,
        caller: Option<&'a Caller>,
        file: &'a Path,
    ) -> BoxFuture<'a, Result<Body>>;

    /// Claims the space for this machine's import of the move `ticket`.
    fn claim_moved<'a>(
        &'a self,
        id: &'a SpaceId,
        ticket: &'a MoveTicket,
    ) -> BoxFuture<'a, Result<(), ClaimFailure>>;

    /// Claims the space for this machine's import as another machine moves
    /// it here.
    fn claim_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()>;

    /// Claims the space for this machine's takeover from the package the
    /// hub keeps.
    fn claim_taken_over<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<(), ClaimFailure>>;

    /// Whether a space above the space on another machine has an agent,
    /// which may work in it.
    fn coordinator_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, bool>;

    /// Deepest first.
    fn descendants_elsewhere<'a>(&'a self, id: &'a SpaceId)
    -> BoxFuture<'a, Result<Vec<SpaceRef>>>;

    fn ensure_parent_elsewhere<'a>(
        &'a self,
        space: Option<&'a SpaceId>,
        repository: Option<&'a RepositoryId>,
        parent: &'a SpaceId,
    ) -> BoxFuture<'a, Result<()>>;

    /// `file` must not exist yet.
    fn fetch_package<'a>(&'a self, id: &'a SpaceId, file: &'a Path) -> BoxFuture<'a, Result<()>>;

    fn hub_view<'a>(&'a self, caller: Option<&'a Caller>)
    -> BoxFuture<'a, Result<Option<HubView>>>;

    /// The hub keeps a package of a space whose machine is offline.
    fn kept_package<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<KeptPackage>>>;

    fn listed<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()>;

    /// The hub name, else the host name.
    fn machine_name(&self) -> BoxFuture<'_, String>;

    /// Empty outside a hub.
    fn other_machines(&self) -> BoxFuture<'_, Vec<Member>>;

    /// Counts the threads the space opened on other machines that wait for
    /// their holder, as the hub lists them.
    fn waiting_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, usize>;

    /// The hub holds the operation until `machine` can be reached. Returns
    /// an error `machine` answered.
    fn tell_machine<'a>(
        &'a self,
        machine: &'a Member,
        operation: Operation,
    ) -> BoxFuture<'a, Result<()>>;

    /// As the hub's directory lists it.
    fn machine_of<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<(Member, Listed)>>>;

    /// The machine that keeps the space's removal record, as the hub's
    /// directory lists it.
    fn record_keeper<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<Member>>>;

    /// Matches a machine's ID or a name only one machine has. The flag
    /// says whether it is this machine.
    fn named_machine<'a>(&'a self, named: &'a str) -> BoxFuture<'a, Result<(Member, bool)>>;

    /// `elsewhere` names where it runs, such as "machine desk".
    fn files_stay_here(&self, elsewhere: &str) -> anyhow::Error;

    /// Whether `error` comes from a call through the hub that never left
    /// this side, so the other side never ran it.
    fn never_sent(&self, error: &anyhow::Error) -> bool;

    fn origin_url_elsewhere<'a>(&'a self, id: &'a SpaceId)
    -> BoxFuture<'a, Result<Option<String>>>;

    /// None unless the parent runs on another machine.
    fn parent_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<SpaceRef>>>;

    fn place_start(&self, request: Request) -> BoxFuture<'_, Result<Option<Decision>>>;

    fn refuse_live_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>>;

    /// Removes only the spaces directly below, as `below` asks, and returns
    /// their names.
    fn remove_children_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
        below: BelowElsewhere<'a>,
    ) -> BoxFuture<'a, Result<Vec<SpaceName>>>;

    fn repository_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<SpaceRepository>>;

    /// Returns whether `machine` claimed the space.
    fn settle_move<'a>(
        &'a self,
        id: &'a SpaceId,
        machine: &'a MemberId,
        ticket: &'a MoveTicket,
    ) -> BoxFuture<'a, Result<bool>>;

    fn this_machine(&self) -> BoxFuture<'_, Result<Option<Machine>>>;
}

/// The CI jobs this machine's runner runs in workspaces.
pub trait Runner: Send + Sync {
    fn attach_jobs<'a>(&'a self, spaces: &'a mut [Space]) -> BoxFuture<'a, Result<()>>;

    /// Cancels the job before its runner stops, as `why` stops it.
    fn cancel_job_before_stopping<'a>(
        &'a self,
        id: &'a SpaceId,
        why: shoal_daemon_base::jobs::JobStop,
    ) -> BoxFuture<'a, ()>;

    fn linked_job_workspaces<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<Workspace>>>;

    fn linked_waiting_jobs<'a>(&'a self, id: &'a SpaceId)
    -> BoxFuture<'a, Result<Vec<QueueEntry>>>;

    fn workspace_job<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<Box<QueuedJob>>>>;
}

/// The reviews of workspaces, and the gate agent.
pub trait Reviews: Send + Sync {
    /// Refuses `caller` where the setting for `gate` holds it, unless a
    /// finished review covers `head`.
    fn check_review<'a>(
        &'a self,
        workspace: &'a Workspace,
        gate: ReviewGate,
        head: &'a CommitId,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>>;

    /// [`Self::check_review`], and queues the missing review.
    fn require_review<'a>(
        &'a self,
        workspace: &'a Workspace,
        gate: ReviewGate,
        head: &'a CommitId,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Queues a review of the workspace when `error` refuses for want of
    /// one, and says so in the error it returns.
    fn queue_unreviewed(&self, error: anyhow::Error) -> BoxFuture<'_, anyhow::Error>;

    /// Refuses without asking the gate when the gate decides `request` and
    /// has denied the same action too often.
    fn ensure_gate_hears<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>>;

    /// The review a package carries to the machine that imports the
    /// workspace.
    fn pending_review<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<CarriedReview>>>;

    fn queue_carried_review<'a>(
        &'a self,
        id: &'a SpaceId,
        review: CarriedReview,
    ) -> BoxFuture<'a, Result<()>>;

    /// Stops the answers made for the review too.
    fn stop_reviews<'a>(&'a self, workspace_id: &'a SpaceId) -> BoxFuture<'a, ()>;
}

/// The ports, simulators, containers and resource leases spaces hold.
pub trait Leases: Send + Sync {
    fn container_project<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<ContainerProject>>>;

    /// Of every space when `selector` is `None`.
    fn list_resources<'a>(
        &'a self,
        selector: Option<&'a SpaceSelector>,
    ) -> BoxFuture<'a, Result<Vec<ResourceLease>>>;

    fn list_simulators<'a>(
        &'a self,
        owner: Option<&'a SpaceId>,
    ) -> BoxFuture<'a, Result<Vec<Simulator>>>;

    /// Takes the volumes down too.
    fn remove_containers<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>>;

    fn remove_simulators<'a>(&'a self, owner: &'a SpaceId) -> BoxFuture<'a, Result<()>>;

    /// Each running lease comes with when it stops. Empty unless the agent
    /// is asleep.
    fn kept_leases<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Vec<KeptLease>>>;

    /// A failure keeps the lease.
    fn run_resource_release_hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        lease: &'a ResourceLease,
        command: Option<&'a Path>,
    ) -> BoxFuture<'a, Result<()>>;
}

/// The demos spaces run.
pub trait Demos: Send + Sync {
    /// Stops every demo of the space when `name` is `None`.
    fn stop_demos<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        name: Option<String>,
    ) -> BoxFuture<'a, Result<()>>;
}

/// What other subsystems record of a space. The spaces read and write it in
/// their own transactions.
pub trait Records: Send + Sync {
    /// Fills in its latest review, its agent's state, sleep, wake, restart
    /// and place in the queue, and its links.
    fn load_workspace(&self, db: &Connection, workspace: &mut Workspace) -> Result<()>;

    /// Fills in its agent's state, sleep, wake, restart and place in the
    /// queue, and its links.
    fn load_chat(&self, db: &Connection, chat: &mut Chat) -> Result<()>;

    fn assigned_items(
        &self,
        db: &Connection,
        id: &SpaceId,
        kind: Option<shoal_forge_types::items::link::ItemKind>,
    ) -> Result<Vec<ItemRef>>;

    fn recorded_items(
        &self,
        db: &Connection,
        id: &SpaceId,
        relation: Relation,
    ) -> Result<Vec<LinkedItem>>;

    fn space_links(&self, db: &Connection, id: &SpaceId) -> Result<SpaceLinks>;

    fn agent_state(
        &self,
        db: &Connection,
        id: &SpaceId,
    ) -> Result<Option<shoal_protocol::state::AgentState>>;

    /// Hands the alerts the agent did not read to the user, as removal does.
    /// Returns the alert the user got for them.
    fn hand_alerts_to_user(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<i64>>;

    /// Forgets the logins the removed space stood for on its issues, and its
    /// issue links, and returns them.
    fn remove_issue_links(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
    ) -> Result<(Vec<Dropped>, Vec<ItemRef>)>;

    /// Drops the pending reviews the removed space writes, or of it, with
    /// their comments.
    fn drop_pending_reviews(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()>;

    /// Cancels the turn retry too.
    fn cancel_wake(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()>;

    fn turn_retry(&self, db: &Connection, id: &SpaceId) -> Result<Option<TurnRetry>>;

    /// Of `space`, or of every space when it is `None`.
    fn other_executions(
        &self,
        db: &Connection,
        space: Option<&SpaceId>,
        own: Option<&ExecutionId>,
    ) -> Result<Vec<(SpaceId, ExecutionId)>>;

    /// Leaves out those whose restore waits in the queue.
    fn running_agents(&self, db: &Connection, id: &SpaceId) -> Result<Vec<ExecutionId>>;

    /// From the space's newest queue entry.
    fn queued_agent(&self, db: &Connection, id: &SpaceId) -> Result<Option<Agent>>;

    fn launch_waits_to_open(&self, db: &Connection, id: &SpaceId) -> Result<bool>;

    /// Forgets the queue entries that restore or opened them too, as the
    /// space's kind changes under it.
    fn forget_executions(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()>;

    fn forget_command_executions(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()>;

    /// Marks what other subsystems recorded as running when the daemon
    /// stopped as interrupted: executions and simulator clean requests.
    fn interrupt_operations(&self, tx: &Transaction<'_>) -> Result<()>;

    fn repositories(&self, db: &Connection) -> Result<Vec<Repository>>;

    fn touch_repository(&self, tx: &Transaction<'_>, id: &RepositoryId) -> Result<()>;

    fn leasing_workspaces(&self, db: &Connection, name: &str) -> Result<Vec<SpaceId>>;

    /// None on a machine that does not number its spaces from a hub's
    /// sequence.
    fn next_space_number(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<SpaceNumber>>;

    /// Refuses while alerts hold more files than fit.
    fn ensure_holders_fit(&self, tx: &Transaction<'_>, paths: &[PathBuf]) -> Result<()>;

    fn coordination(&self) -> &dyn shoal_coordination::daemon::Records;
}

/// The agent a request comes from, and the space it runs in.
pub struct CallerAgent {
    pub execution: ExecutionId,
    pub name: Agent,
    pub space_id: SpaceId,
}

/// Why claiming a space that a move through the hub brings failed.
#[derive(Debug)]
pub enum ClaimFailure {
    Refused(anyhow::Error),
    /// The hub may have taken the claim, as when its answer never came.
    Unknown(anyhow::Error),
}

/// What a removal asks of the spaces directly below its space that other
/// machines run. Each machine removes its own as it removes a child.
pub struct BelowElsewhere<'a> {
    /// Leave the spaces the user holds, as a workspace process's removal
    /// does.
    pub refuse_holds: bool,
    /// When the caller confirmed the spaces it saw, the only ones the
    /// removal takes.
    pub confirmed: Option<&'a [SpaceId]>,
    pub cause: EventCause,
}

/// Where the workspaces stacked on a base go once its work merged.
pub struct Restack<'a> {
    pub base: &'a Workspace,
    /// Stacked branches rebase past this commit.
    pub head: &'a CommitId,
    /// Stacked PRs that still target one of these are retargeted.
    pub merged_heads: &'a [BranchName],
    /// Stacked branches rebase onto it and their PRs target it.
    pub target: &'a BranchName,
    pub onto: Onto,
    /// None for the default branch.
    pub new_base: Option<SpaceId>,
    pub cause: EventCause,
    pub account: &'a Account,
}

/// Where a stacked branch finds the branch its base merged into.
#[derive(Clone, Copy)]
pub enum Onto {
    /// Fetched first.
    Remote,
    Local,
    /// The stacked branch has the base's work already and needs no rebase.
    Held,
}

/// A merge that `shoal merge` pushed to the default branch's upstream.
pub struct PushedMerge {
    pub space: SpaceId,
    pub remotes: Vec<String>,
    pub branch: BranchName,
    pub commit: CommitId,
}

#[derive(Debug, Clone)]
pub struct OpenIssue {
    pub url: ItemRef,
    pub title: String,
    pub labels: Vec<String>,
}

/// The login a space stood for among an issue's assignees until it dropped
/// the issue's link.
#[derive(Debug, Clone)]
pub struct Dropped {
    pub url: String,
    pub login: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewGate {
    Ready,
    Merge,
}

/// Everything the spaces need of the rest of the daemon. Each operation that
/// reaches beyond the spaces takes it.
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
    fn spaces(&self) -> &Spaces;

    fn coordination(&self) -> &Coordination;
}
