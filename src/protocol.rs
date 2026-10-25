//! Newline-delimited JSON over the daemon's Unix socket. Bump [`VERSION`]
//! whenever a request, response, or event changes shape or spelling.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::{
    config::repo::{ConfigLayers, LocalConfig},
    daemon::{
        allocation::Allocation,
        events::EventItem,
        notifications::Notification,
        ports::PortRequest,
        recovery::{ReconcileOptions, Report},
        resources::{Overview, ResourceLease, ResourceRequest},
        workspace::ExecutionKind,
    },
    hooks::HookKind,
    model::{
        ConflictCheck, DiffBase, ExecutionPlan, Inspection, PortOverview, PortReservation,
        PortSuggestion, Repository, RepositoryRemoval, SyncedRepository, Workspace,
        WorkspaceStatus,
    },
    process::identity::Identity,
    removal::{RemovalCheck, RemovalResult, UnsavedWork},
    sim::{SimRequest, Simulator, SimulatorCatalog, audit::AuditEntry},
};

pub const VERSION: u32 = 135;
pub const MAX_FRAME: usize = 64 * 1024;

/// Shared CLI, daemon, and wrapper timing; keep related budgets in view when tuning.
pub mod timing {
    use std::time::Duration;

    /// Release a daemon connection slot if the client never sends its request.
    pub const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
    /// Status and shutdown should fail promptly when the daemon is unresponsive.
    pub const ADMIN_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
    /// Ordinary requests may include slow Git, simulator, or hook work.
    pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(3600);
    /// Allow daemon startup or shutdown to settle across repeated status requests.
    pub const DAEMON_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
    /// A long-lived wait that lost its daemon gives the next one this long to
    /// listen and know the caller's scope again: the service manager's restart
    /// throttle, the startup audit, and the wrapper's reattachment.
    pub const DAEMON_RESTART_TIMEOUT: Duration = Duration::from_secs(120);
    /// Observe daemon transitions promptly without busy-polling the socket.
    pub const DAEMON_POLL_INTERVAL: Duration = Duration::from_millis(100);
    /// A managed daemon restarts with an installed update once its running
    /// operations finish, which can include a long setup.
    pub const UPDATE_WAIT_TIMEOUT: Duration = Duration::from_secs(600);
    /// Each refused attempt briefly holds back the update it waits for.
    pub const UPDATE_POLL_INTERVAL: Duration = Duration::from_secs(1);

    /// Ordinary command registration needs no slow preparation, but its process
    /// and worktree checks run subprocesses; only guard against a hung daemon.
    pub const EXECUTION_START_TIMEOUT: Duration = Duration::from_secs(30);
    /// Setup and landing can wait for Git gates, hooks, and upstream refreshes.
    pub const PREPARED_EXECUTION_START_TIMEOUT: Duration = Duration::from_secs(120);
    /// Wait for the daemon to persist the spawned child's process group.
    pub const START_ACK_TIMEOUT: Duration = Duration::from_secs(10);
    /// Cover stopping an exited agent's survivors and the completion scan,
    /// with room for ownership checks and persistence.
    pub const COMPLETION_ACK_TIMEOUT: Duration = Duration::from_secs(60);
    /// Bound the ps inventory subprocess used by the daemon's completion scan.
    pub const PROCESS_INVENTORY_TIMEOUT: Duration = Duration::from_secs(10);
    /// Give an identity-verified survivor time to handle SIGTERM before SIGKILL.
    pub const PROCESS_TERM_GRACE: Duration = Duration::from_secs(2);
    /// Wait for a survivor to disappear after SIGKILL before reporting it.
    pub const PROCESS_KILL_WAIT: Duration = Duration::from_secs(1);
    /// Wait within one scan for processes caught mid-exec to publish their environment.
    pub const PROCESS_SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
    /// Cover ordinary EXECUTION_START_TIMEOUT plus START_ACK_TIMEOUT and wrapper startup.
    pub const DETACHED_LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);
    /// Collect a failed detached wrapper's exit status without waiting indefinitely.
    #[cfg_attr(not(feature = "happy"), allow(dead_code))]
    pub const DETACHED_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
    /// Give the child time to handle a stop signal before escalating to SIGKILL.
    pub const EXECUTION_STOP_GRACE: Duration = Duration::from_secs(2);
    /// Allow wrappers to stop beyond EXECUTION_STOP_GRACE and report completion.
    pub const WORKSPACE_STOP_TIMEOUT: Duration = Duration::from_secs(10);
    /// Notice completed execution records promptly while workspace stopping waits.
    pub const WORKSPACE_STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);
    /// First retry of a detached wrapper; the delay doubles up to the cap.
    pub const REATTACH_RETRY_INITIAL: Duration = Duration::from_millis(100);
    /// Keep a detached wrapper's scope gap short once the daemon returns.
    pub const REATTACH_RETRY_MAX: Duration = Duration::from_secs(1);
    /// Give up on an unresponsive daemon's reattach reply and retry.
    pub const REATTACH_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
    /// After its command exits, wait this long for a restarting daemon to take
    /// the exit report; cover a stop with its workspace drain plus a start.
    pub const REATTACH_EXIT_TIMEOUT: Duration = Duration::from_secs(60);

    const _: () = {
        // Each pass scans markers, then inspects related processes with a
        // second inventory; an exited agent gets a stop pass before completion.
        assert!(
            COMPLETION_ACK_TIMEOUT.as_millis()
                > 2 * (2 * PROCESS_INVENTORY_TIMEOUT.as_millis()
                    + PROCESS_SETTLE_TIMEOUT.as_millis())
                    + PROCESS_TERM_GRACE.as_millis()
                    + PROCESS_KILL_WAIT.as_millis()
        );
        assert!(
            DETACHED_LAUNCH_TIMEOUT.as_millis()
                > EXECUTION_START_TIMEOUT.as_millis() + START_ACK_TIMEOUT.as_millis()
        );
        assert!(WORKSPACE_STOP_TIMEOUT.as_millis() > EXECUTION_STOP_GRACE.as_millis());
        assert!(
            REATTACH_EXIT_TIMEOUT.as_millis()
                > WORKSPACE_STOP_TIMEOUT.as_millis() + 2 * DAEMON_WAIT_TIMEOUT.as_millis()
        );
        assert!(
            DAEMON_RESTART_TIMEOUT.as_millis()
                > DAEMON_WAIT_TIMEOUT.as_millis() + REATTACH_RETRY_MAX.as_millis()
        );
    };
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub id: u64,
    pub method: Method,
    #[serde(default)]
    pub scope: Option<String>,
}

impl Request {
    /// A request from this process, carrying its workspace scope token if any.
    pub fn new(method: Method) -> Self {
        Self {
            protocol: VERSION,
            id: 1,
            method,
            scope: crate::env::scope_token(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    // Daemon administration.
    Status,
    Shutdown,
    ReloadConfig,
    // Repositories.
    ListRepositories,
    RegisterRepository {
        source: crate::repository::Selector,
        name: Option<String>,
        path: Option<std::path::PathBuf>,
    },
    /// Clone a repository URL for `shoal contribute`.
    RegisterContribution {
        source: String,
        name: Option<String>,
    },
    RenameRepository {
        repository: crate::repository::Selector,
        name: String,
    },
    RepositoryConfig {
        repository: crate::repository::Selector,
    },
    SetRepositoryConfig {
        repository: crate::repository::Selector,
        toml: Option<String>,
    },
    EditRepositoryConfig {
        repository: crate::repository::Selector,
        changes: Vec<crate::config::edit::Change>,
    },
    RemoveRepository {
        repository: crate::repository::Selector,
    },
    /// The repositories `MoveRepositories` would move into `root`, or the
    /// configured `root_dir`, after the checks it runs before moving any.
    PlanRepositoryMoves {
        root: Option<std::path::PathBuf>,
    },
    /// Move every repository directory into the configured `root_dir`.
    MoveRepositories,
    /// Fetch the default branch's remote and fast-forward the local default branch.
    SyncRepository {
        repository: crate::repository::Selector,
    },
    // Workspaces.
    ListBranches {
        repository: crate::repository::Selector,
    },
    OpenBranch {
        path: Option<std::path::PathBuf>,
        repository: crate::repository::Selector,
        branch: String,
        git_profile: Option<String>,
        base: Option<String>,
        /// The chat opening the workspace; `None` gives it to the user.
        chat: Option<String>,
    },
    CreateWorkspace {
        path: Option<std::path::PathBuf>,
        repository: crate::repository::Selector,
        name: String,
        base: Option<String>,
        git_profile: Option<String>,
        /// The chat opening the workspace; `None` gives it to the user.
        chat: Option<String>,
    },
    /// Reopen the existing `workspace` for a launch in it, never creating one.
    ReopenWorkspace {
        workspace: String,
    },
    /// Create a workspace on a new branch in `workspace`'s repository, split
    /// from it as its child.
    SplitWorkspace {
        workspace: String,
        name: String,
        base: Option<String>,
    },
    /// Refuse what `CreateWorkspace` or `OpenBranch` would refuse in this
    /// request, without creating anything.
    CheckCreation {
        repository: crate::repository::Selector,
        /// The new branch; `existing` names a branch to open instead.
        branch: Option<String>,
        existing: Option<String>,
        base: Option<String>,
        git_profile: Option<String>,
        path: Option<std::path::PathBuf>,
    },
    /// Refuse the work `after` names that a queued launch in `repository`
    /// could not wait for, without queuing anything.
    CheckDependencies {
        repository: crate::repository::Selector,
        after: Vec<crate::model::Dependency>,
    },
    AdoptWorkspace {
        repository: crate::repository::Selector,
        path: std::path::PathBuf,
        copy: bool,
    },
    RenameWorkspace {
        workspace: String,
        branch: String,
    },
    ListWorkspaces,
    // Chats in registered checkouts.
    /// Record a chat whose agent replaces process `pid`.
    StartChat {
        repository: crate::repository::Selector,
        agent: String,
        pid: u32,
    },
    /// Record that `chat`'s agent now runs as process `pid`, for an agent
    /// started detached instead of in place of the recording process.
    MoveChat {
        chat: String,
        pid: u32,
    },
    ListChats,
    // The agent queue under `max_agents`.
    QueueAdd {
        request: crate::model::QueueRequest,
    },
    ListQueue,
    /// The plan a queued start runs.
    QueuedPlan {
        entry: String,
    },
    /// Remove a waiting entry, named by ID or workspace name.
    QueueRemove {
        entry: String,
    },
    /// Keep a starting entry's slot while the connection stays open.
    QueueHold {
        entry: String,
    },
    /// Record the workspace a starting entry opened.
    QueueAttach {
        entry: String,
        workspace: String,
    },
    /// End a starting entry that could not start its agent.
    QueueFail {
        entry: String,
        error: String,
    },
    HoldAcquire {
        workspace: String,
        name: String,
        reason: Option<String>,
    },
    HoldRelease {
        workspace: String,
        name: String,
    },
    HoldList {
        workspace: String,
    },
    /// Running demos, of one workspace or, unscoped, of every workspace.
    ListDemos {
        workspace: Option<String>,
    },
    /// How far the start of one demo launch in a workspace got.
    DemoProgress {
        workspace: String,
        launch: String,
    },
    /// Ask a workspace's demos to stop: the one named, or every one.
    StopDemos {
        workspace: String,
        name: Option<String>,
    },
    /// Record or clear the workspace whose branch this one builds on.
    SetBaseWorkspace {
        workspace: String,
        base: Option<String>,
    },
    WorkspaceUndone {
        workspace: String,
    },
    /// Forget the workspace's stopped work, only `execution`'s when given,
    /// or else its last session.
    DiscardStoppedWork {
        workspace: String,
        execution: Option<String>,
    },
    /// `cleanup` requests cleanup even when `[done] cleanup` is false.
    WorkspaceDone {
        workspace: String,
        cleanup: bool,
    },
    MarkReady {
        workspace: String,
        selection: crate::forge::items::link::Selection,
    },
    /// `draft` overrides `[unready] draft` when set.
    ClearReady {
        workspace: String,
        selection: crate::forge::items::link::Selection,
        draft: Option<bool>,
    },
    /// The caller's execution, when scoped, is recorded as the reporter,
    /// and `session` as the agent session it resumes.
    SetAgentState {
        workspace: String,
        state: crate::state::AgentState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    SetIssue {
        workspace: String,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Link an issue or PR by number or URL with a relation.
    LinkItem {
        workspace: String,
        kind: crate::forge::items::link::ItemKind,
        url: String,
        relation: crate::model::Relation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Unlink the relation's items, optionally only one kind or one item.
    UnlinkItems {
        workspace: String,
        relation: crate::model::Relation,
        kind: Option<crate::forge::items::link::ItemKind>,
        url: Option<String>,
    },
    SetPr {
        workspace: String,
        #[serde(flatten)]
        action: crate::daemon::links::pr::Action,
    },
    PrWait {
        workspace: String,
        timeout_secs: Option<u64>,
    },
    WatchItems {
        workspace: String,
        selection: crate::forge::items::link::Selection,
        timeout_secs: Option<u64>,
    },
    /// Long-lived: answers once whether the workspace's agent is idle with
    /// undelivered messages or queued item updates matches `due`.
    AwaitAgentWake {
        workspace: String,
        due: bool,
    },
    AcknowledgePrUpdates {
        workspace: String,
        deliveries: Vec<String>,
        /// Agent messages the watch printed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        messages: Vec<i64>,
    },
    InspectWorkspace {
        workspace: String,
    },
    /// The workspace record once its worktree identity is verified, and with
    /// `on_branch` that it is on its recorded branch, before the CLI changes Git
    /// state through it.
    VerifyWorkspace {
        workspace: String,
        on_branch: bool,
    },
    /// Fast-forward the workspace recorded on `branch` to the fetched
    /// `origin/<branch>`, refusing a worktree that is not the recorded one on it.
    FastForwardWorkspace {
        workspace: String,
        branch: String,
    },
    /// Link the open PR of the workspace's branch, or open one and link it.
    OpenPull {
        workspace: String,
        options: crate::forge::items::create::PullOptions,
    },
    /// Open an issue in the workspace's repository, or the one `repository`
    /// names, linking it when `link` is set.
    OpenIssue {
        workspace: String,
        issue: crate::forge::items::create::NewIssue,
        repository: Option<String>,
        link: bool,
    },
    /// Change an issue or PR of the workspace's repository: the one `item`
    /// names, or the workspace's linked one of `kind`.
    ItemAction {
        workspace: String,
        kind: crate::forge::items::link::ItemKind,
        item: Option<String>,
        action: crate::forge::items::action::Action,
    },
    WorkspaceEnv {
        workspace: String,
    },
    RevokeWorkspaceEnv {
        workspace: String,
        token: String,
    },
    WorkspaceStatus {
        workspace: String,
    },
    /// Issues or PRs of the workspace's repository that `search` matches,
    /// answered in one or more `items` frames and then `ok`.
    ListItems {
        workspace: String,
        kind: crate::forge::items::link::ItemKind,
        search: crate::forge::items::list::Search,
    },
    /// A request to the workspace repository's forge API, answered with
    /// the response's JSON text in one or more `api_response` frames and
    /// then `ok`.
    ForgeApi {
        workspace: String,
        request: crate::forge::raw::RawRequest,
    },
    /// Linked items, or an explicit one, for the CLI to look up.
    SelectItems {
        workspace: String,
        selection: crate::forge::items::link::Selection,
    },
    CheckLog {
        workspace: String,
        selection: crate::forge::items::link::Selection,
        check: String,
    },
    /// Rerun failed or cancel running checks on a PR's head; none named means all.
    CiAction {
        workspace: String,
        item: Option<String>,
        action: crate::forge::ci::CiAction,
        checks: Vec<String>,
    },
    /// Workspaces that link an item or hold a resource.
    FindWorkspaces {
        target: crate::model::WorkspaceTarget,
    },
    StopWorkspace {
        workspace: String,
    },
    CheckRemoval {
        workspace: String,
        #[serde(default)]
        include_changes: bool,
    },
    RemoveWorkspace {
        workspace: String,
        unsaved: UnsavedWork,
    },
    /// Remove idle and merged cleanup candidates without waiting for their
    /// idle delay, and empty shared caches nothing is using.
    Cleanup {
        dry_run: bool,
        #[serde(flatten)]
        selection: crate::daemon::cleanup::Selection,
    },
    /// Removed workspaces that can still be restored.
    RemovedWorkspaces,
    /// Where the tracked agents of the repository with this ID kept their
    /// configuration, also once it is removed; in pages.
    AgentEnvironments {
        repository_id: String,
    },
    /// Forget [`Method::AgentEnvironments`] once a removed repository's trust
    /// entries there are clean.
    ForgetAgentEnvironments {
        repository_id: String,
    },
    /// Branches removal kept because only they have some commits, only
    /// those of one repository when a selector names it.
    KeptBranches {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<String>,
    },
    /// Delete kept branches with the commits only they have; all or none.
    DeleteKeptBranches {
        branches: Vec<crate::model::BranchTarget>,
    },
    /// Open a removed workspace again from its removal record.
    RestoreWorkspace {
        workspace: String,
    },
    /// Remove a workspace, or take a removed one, and write it for another
    /// Shoal to the new file `file`.
    ExportWorkspace {
        workspace: String,
        unsaved: UnsavedWork,
        file: std::path::PathBuf,
    },
    /// Record a workspace another Shoal exported as removed here, with its
    /// checkpoint fetched from `bundle`, and open it again.
    ImportWorkspace {
        repository: crate::repository::Selector,
        bundle: Option<std::path::PathBuf>,
        prerequisite: Option<String>,
        workspace: Box<crate::model::RemovedWorkspace>,
    },
    /// Keep the workspace's HEAD and unsaved files in a new checkpoint.
    Checkpoint {
        workspace: String,
        name: Option<String>,
    },
    /// A page of an existing workspace's checkpoints numbered after `after`;
    /// none when no workspace matches.
    Checkpoints {
        workspace: String,
        after: Option<u64>,
    },
    /// Return a workspace to one of its checkpoints.
    RestoreCheckpoint {
        workspace: String,
        checkpoint: u64,
    },
    Diagnose,
    /// The forge `host` serves, asking it over `scheme` once when neither
    /// the global config nor a recorded answer says.
    ForgeType {
        host: String,
        scheme: String,
    },
    Doctor {
        workspace: Option<String>,
        options: ReconcileOptions,
    },
    /// Verify that this worker belongs to an unscoped caller's landing execution.
    CheckLanding,
    DiffBase {
        workspace: String,
    },
    Conflicts {
        workspace: String,
        target: Option<String>,
    },
    /// Keep where an agent of the workspace keeps its configuration, before
    /// Shoal trusts the workspace there, until the repository's removal.
    KeepAgentEnvironment {
        workspace: String,
        environment: std::collections::BTreeMap<String, String>,
    },
    /// [`Method::KeepAgentEnvironment`] for a chat in the repository's
    /// checkout, which belongs to no workspace.
    KeepRepositoryAgentEnvironment {
        repository: String,
        environment: std::collections::BTreeMap<String, String>,
    },
    /// One effective hook, resolved against the directory where it runs.
    WorkspaceHook {
        workspace: String,
        kind: HookKind,
    },
    /// The repository layer of the target's config, for the CLI to resolve
    /// against the global config it reads at launch.
    LayeredConfig {
        target: ConfigTarget,
    },
    /// Long-lived: the connection stays open for the execution's lifetime.
    /// `agent` names a Shoal agent shortcut whose exit the user is told about.
    Execute {
        workspace: String,
        wrapper: Identity,
        kind: ExecutionKind,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        recover: bool,
        /// The wrapper keeps its command running when the daemon goes away
        /// and returns with [`Method::Reattach`].
        #[serde(default)]
        reattach: bool,
        /// The agent queue entry whose slot this agent takes over.
        #[serde(default)]
        queue_entry: Option<String>,
        /// The demo this command runs, recorded with its execution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        demo: Option<Box<crate::demos::Launch>>,
    },
    /// Long-lived: an execution connection resumed after a daemon restart.
    Reattach(Reattach),
    // Notifications.
    ListNotifications {
        unread_only: bool,
        limit: u32,
    },
    MarkNotificationsRead {
        ids: Vec<i64>,
    },
    /// Deletes every notification, read or not; answers with how many.
    ClearNotifications,
    /// A message from a workspace process to the user.
    SendMessage {
        workspace: String,
        message: String,
    },
    /// A message from the user to a workspace's agents.
    SendAgentMessage {
        workspace: String,
        message: String,
    },
    /// The workspace's undelivered agent messages, oldest first.
    AgentMessages {
        workspace: String,
    },
    MarkAgentMessagesDelivered {
        workspace: String,
        ids: Vec<i64>,
    },
    /// A question from a workspace's agents to the user; asking an identical
    /// question again returns it until its answer is delivered.
    AskQuestion {
        workspace: String,
        ask: crate::daemon::questions::Ask,
    },
    /// Open questions, in every workspace unless one is named.
    ListQuestions {
        workspace: Option<String>,
    },
    /// One question by ID, for the user.
    ShowQuestion {
        id: i64,
    },
    /// Withdraw the workspace's pending approval request `id`, which its
    /// agent no longer needs.
    WithdrawApproval {
        workspace: String,
        id: i64,
    },
    /// Delete the unread message `id` the workspace sent the user.
    WithdrawMessage {
        workspace: String,
        id: i64,
    },
    /// Close the workspace's open question `id`, which its agents no longer
    /// need answered.
    WithdrawQuestion {
        workspace: String,
        id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<crate::daemon::questions::Question>,
    },
    /// The user's answer, queued for the asking workspace's agents.
    /// `shown` is the question the user was shown, which a later question
    /// reusing its number does not match.
    AnswerQuestion {
        id: i64,
        answer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<crate::daemon::questions::Question>,
    },
    /// Accept the stored recommendation literally, including numeric text.
    AcceptQuestionRecommendation {
        id: i64,
        shown: Option<crate::daemon::questions::Question>,
    },
    /// Long-lived: the question once it is answered. What it asks tells it
    /// from a later question reusing its number.
    AwaitAnswer {
        workspace: String,
        id: i64,
        ask: crate::daemon::questions::Ask,
    },
    /// The workspace's undelivered agent messages and queued item updates,
    /// without waiting or polling the forge; acknowledge them with
    /// [`Method::AcknowledgePrUpdates`]. It replaces a running watch.
    AgentUpdates {
        workspace: String,
    },
    /// The chat's undelivered messages about the workspaces it coordinates,
    /// without waiting; acknowledge them with
    /// [`Method::AcknowledgeChatMessages`]. It replaces a running wait.
    ChatUpdates {
        chat: String,
    },
    /// Long-lived: the chat's next messages, once it has any.
    WaitChatMessages {
        chat: String,
    },
    AcknowledgeChatMessages {
        chat: String,
        ids: Vec<i64>,
    },
    /// Long-lived: unread notifications after `after`, then new ones as they
    /// are recorded, each as a [`Body::Notification`] response and marked
    /// read on delivery unless `keep_unread`.
    WatchNotifications {
        keep_unread: bool,
        after: i64,
    },
    WatchWorkspaceEvents {
        since: Option<i64>,
        follow: bool,
    },
    // Ports.
    PortAcquire {
        workspace: String,
        name: String,
        request: PortRequest,
    },
    PortRelease {
        workspace: String,
        name: String,
    },
    PortOverview {
        workspace: String,
    },
    PortCatalog {
        workspace: String,
    },
    ListApprovals {
        workspace: Option<String>,
    },
    ShowApproval {
        id: i64,
    },
    /// Confirm the caller may fetch a configured secret, requesting approval
    /// when it is gated; the daemon never sees the value.
    SecretAcquire {
        workspace: String,
        name: String,
        request: crate::daemon::secrets::SecretRequest,
    },
    SecretRelease {
        workspace: String,
        name: String,
    },
    /// `shown` is the request the user was shown, which a later request
    /// reusing its number does not match.
    DecideApproval {
        id: i64,
        approve: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<Box<crate::daemon::approvals::ApprovalRequest>>,
        /// Edits, a reason or a duplicate, for proposals only.
        #[serde(default)]
        details: crate::daemon::approvals::proposals::ProposalDecision,
    },
    /// Pass a request the coordinating agent of `workspace` decides on to the
    /// next coordinating agent above it that holds a grant, or to the user.
    EscalateApproval {
        workspace: String,
        id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<Box<crate::daemon::approvals::ApprovalRequest>>,
    },
    // Cooperative resources.
    ResourceAcquire {
        workspace: String,
        request: ResourceRequest,
    },
    ResourceRelease {
        workspace: String,
        pool: String,
        name: String,
    },
    ResourceOverview {
        workspace: String,
    },
    // Simulators.
    SimCatalog,
    SimOverview {
        workspace: Option<String>,
    },
    /// Lease records for completion, without loading repository configuration.
    SimList {
        workspace: Option<String>,
    },
    SimAcquire {
        workspace: String,
        request: SimRequest,
    },
    SimRelease {
        workspace: String,
        name: String,
    },
    SimHistory {
        workspace: Option<String>,
        limit: u32,
        before: Option<i64>,
    },
}

impl Method {
    /// The variant's protocol name, for timing lines.
    pub fn name(&self) -> String {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(name)) => name,
            Ok(serde_json::Value::Object(fields)) => fields
                .into_iter()
                .next()
                .map(|(name, _)| name)
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Daemon administration, which must reach a daemon of any release.
    pub fn controls_daemon(&self) -> bool {
        matches!(self, Self::Status | Self::Shutdown | Self::ReloadConfig)
    }

    /// Executions, waits and streams, which keep the connection open after
    /// the request.
    pub fn holds_connection(&self) -> bool {
        matches!(
            self,
            Self::Execute { .. }
                | Self::Reattach(_)
                | Self::WatchNotifications { .. }
                | Self::QueueHold { .. }
                | Self::WatchWorkspaceEvents { .. }
                | Self::PrWait { .. }
                | Self::AwaitAgentWake { .. }
                | Self::AwaitAnswer { .. }
                | Self::WatchItems { .. }
                | Self::WaitChatMessages { .. }
        )
    }

    /// Long-lived requests whose clients send them again when the daemon
    /// closes the connection unanswered, as across a restart.
    pub fn retries_when_closed(&self) -> bool {
        matches!(
            self,
            Self::Reattach(_)
                | Self::QueueHold { .. }
                | Self::PrWait { .. }
                | Self::AwaitAgentWake { .. }
                | Self::AwaitAnswer { .. }
                | Self::WatchItems { .. }
                | Self::WaitChatMessages { .. }
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub protocol: u32,
    pub id: u64,
    #[serde(flatten)]
    pub body: Body,
}

impl Response {
    pub fn new(id: u64, body: Body) -> Self {
        Self {
            protocol: VERSION,
            id,
            body,
        }
    }
}

/// Whose repository config: a workspace's worktree file under the saved
/// config, or the registered checkout's before a workspace exists.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigTarget {
    Workspace(String),
    Repository(crate::repository::Selector),
}

impl<T> Allocation<T> {
    pub fn into_body(self, granted: impl FnOnce(T) -> Body) -> Body {
        match self {
            Self::Granted(value) => granted(value),
            Self::Busy(message) => Body::Busy { message },
            Self::Approval(request) => Body::ApprovalRequest(request),
        }
    }
}

crate::state::open_enum! {
    /// Open on receipt so newer daemons can still explain failures.
    ErrorCode {
        InvalidRequest => "invalid_request",
        ProtocolMismatch => "protocol_mismatch",
        ScopeDenied => "scope_denied",
        ManagedService => "managed_service",
        OperationFailed => "operation_failed",
        ExecutionFailed => "execution_failed",
    }
}

// Keep payload conversions and variant names tied to the wire enum.
macro_rules! response_bodies {
    ($($variant:ident($payload:ty),)*) => {
        #[derive(Debug, Serialize, Deserialize)]
        #[serde(tag = "type", content = "data", rename_all = "snake_case")]
        pub enum Body {
            Ok,
            Error { code: ErrorCode, message: String },
            Busy { message: String },
            $($variant($payload),)*
        }

        impl Body {
            pub fn variant_name(&self) -> &'static str {
                match self {
                    Self::Ok => "Ok",
                    Self::Error { .. } => "Error",
                    Self::Busy { .. } => "Busy",
                    $(Self::$variant(_) => stringify!($variant),)*
                }
            }
        }

        $(impl TryFrom<Body> for $payload {
            type Error = anyhow::Error;

            fn try_from(body: Body) -> Result<Self> {
                match body {
                    Body::$variant(value) => Ok(value),
                    body => Err(body.unexpected(stringify!($variant))),
                }
            }
        })*
    };
}

response_bodies! {
    Status(DaemonStatus),
    Repositories(Vec<Repository>),
    Repository(Repository),
    RepositoryConfig(LocalConfig),
    RepositoryRemoved(RepositoryRemoval),
    MovedRepositories(Vec<crate::model::MovedRepository>),
    Workspace(Workspace),
    Hold(crate::model::WorkspaceHold),
    QueueAdmission(crate::model::QueueAdmission),
    QueueEntries(Vec<crate::model::QueueEntry>),
    QueueEntry(crate::model::QueueEntry),
    QueuedPlan(crate::model::QueuedPlan),
    Holds(Vec<crate::model::WorkspaceHold>),
    Demos(Vec<crate::demos::Running>),
    DemoProgress(crate::demos::Progress),
    Workspaces(Vec<Workspace>),
    Chat(crate::model::Chat),
    Chats(Vec<crate::model::Chat>),
    Branches(Vec<crate::git::existing_branch::Branch>),
    OpenedWorkspace(crate::model::OpenedWorkspace),
    Inspection(Inspection),
    Item(crate::forge::items::item::Item),
    Items(Vec<crate::forge::items::item::Item>),
    ApiResponse(Vec<String>),
    Opened(crate::daemon::links::proposals::OpenOutcome),
    Completion(crate::model::Completion),
    Discarded(crate::daemon::workspace::Discarded),
    ReviewMarks(Vec<crate::model::ReviewMark>),
    AgentStatus(crate::model::AgentStatus),
    WorkspaceStatus(WorkspaceStatus),
    SelectedItems(crate::model::Selected),
    CheckLog(crate::forge::ci::checks::CheckLog),
    CiActed(crate::forge::ci::CiActed),
    WorkspaceEnv(std::collections::BTreeMap<String, String>),
    Cleanup(crate::daemon::cleanup::ManualCleanup),
    RemovedWorkspaces(Vec<crate::model::RemovedWorkspace>),
    AgentEnvironments(Vec<std::collections::BTreeMap<String, String>>),
    KeptBranches(Vec<crate::model::KeptBranch>),
    RestoredWorkspace(crate::model::RestoredWorkspace),
    Checkpoint(crate::model::Checkpoint),
    Checkpoints(Option<Vec<crate::model::Checkpoint>>),
    RestoredCheckpoint(crate::model::RestoredCheckpoint),
    FastForwarded(bool),
    Diagnostics(Vec<crate::daemon::doctor::Check>),
    ForgeType(crate::forge::backends::ForgeKind),
    Doctor(Vec<Report>),
    Execution(ExecutionPlan),
    RemovalCheck(RemovalCheck),
    RemovalResult(RemovalResult),
    ExportedWorkspace(crate::transfer::ExportedWorkspace),
    DiffBase(DiffBase),
    Conflicts(ConflictCheck),
    Hook(Option<std::path::PathBuf>),
    LayeredConfig(Box<ConfigLayers>),
    SyncedRepository(SyncedRepository),
    Notifications(Vec<Notification>),
    Cleared(crate::daemon::notifications::Cleared),
    AgentMessages(Vec<crate::daemon::agent_messages::AgentMessage>),
    Question(crate::daemon::questions::Question),
    Questions(crate::daemon::questions::Listing),
    Notification(Notification),
    Sent(crate::daemon::notifications::Sent),
    EventItem(EventItem),
    PrUpdates(crate::daemon::links::wait::Updates),
    Port(PortReservation),
    PortSuggestion(PortSuggestion),
    PortOverview(PortOverview),
    PortCatalog(crate::model::PortCatalog),
    ApprovalRequest(Box<crate::daemon::approvals::ApprovalRequest>),
    ApprovalRequests(Vec<crate::daemon::approvals::ApprovalRequest>),
    SecretGrant(crate::daemon::secrets::SecretGrant),
    ResourceLease(ResourceLease),
    ResourceOverview(Overview),
    Simulator(Simulator),
    Simulators(Vec<Simulator>),
    SimCatalog(SimulatorCatalog),
    SimOverview(crate::sim::SimulatorOverview),
    SimHistory(Vec<AuditEntry>),
}

impl TryFrom<Body> for () {
    type Error = anyhow::Error;

    fn try_from(body: Body) -> Result<Self> {
        match body {
            Body::Ok => Ok(()),
            body => Err(body.unexpected("Ok")),
        }
    }
}

/// A daemon failure, retaining its wire code and message for callers to inspect.
#[derive(Debug)]
pub struct RemoteError {
    pub code: ErrorCode,
    pub message: String,
}

/// `operation_failed` is every failure without a more specific code, so only
/// the specific codes add to the message.
impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.code == ErrorCode::OperationFailed {
            return f.write_str(&self.message);
        }
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RemoteError {}

impl Body {
    /// Preserve remote errors even when the caller expected a different variant.
    pub fn unexpected(self, expected: &str) -> anyhow::Error {
        match self.into_result() {
            Err(error) => error,
            Ok(body) => anyhow::anyhow!(
                "unexpected daemon response; expected {expected}, received {}",
                body.variant_name()
            ),
        }
    }

    pub fn into_result(self) -> Result<Self> {
        match self {
            Self::Error { code, message } => Err(RemoteError { code, message }.into()),
            body => Ok(body),
        }
    }

    pub fn error(code: ErrorCode, error: impl std::fmt::Display) -> Self {
        Self::Error {
            code,
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub version: String,
    pub uptime_secs: u64,
    pub managed: bool,
    pub unread_notifications: u64,
}

/// A wrapper's request to restore its execution connection after the daemon
/// went away. It is permanent: daemons accept it at every protocol version, so
/// wrappers from older releases can reattach after an upgrade. Add fields only
/// with defaults, and keep the execution events and controls such a wrapper
/// understands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reattach {
    pub execution: String,
    pub wrapper: Identity,
    pub child: Option<Identity>,
    pub group_id: u32,
    pub scope_token: String,
    /// Transient agent metadata the daemon lost with its memory.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub recover: bool,
    /// How long the command has run, keeping the overload stop order.
    #[serde(default)]
    pub running_ms: u64,
    /// Set while the wrapper waits to restore an agent that protection
    /// stopped; `child` and `group_id` then name the stopped child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_restore: Option<Protection>,
    /// Set from a demo's report that its child exited until the next run
    /// registers; `child` and `group_id` name the exited child, which the
    /// daemon may already have forgotten.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub between_runs: bool,
}

/// Why protection stopped an agent, and what its automatic restore waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Protection {
    pub reason: String,
    /// Completes "once …"; recovery also waits for every other signal.
    pub resumes_when: String,
}

impl Protection {
    /// Memory or CPU pressure.
    pub(crate) fn load(reason: &str) -> Self {
        Self {
            reason: reason.into(),
            resumes_when: default_resumes_when(),
        }
    }
}

/// Wrapper → daemon messages after an [`Method::Execute`] response.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionEvent {
    Started {
        child: Option<Identity>,
        group_id: u32,
    },
    Finished {
        exit_code: i32,
    },
    Paused,
    /// The child exited and the wrapper starts another, as a demo's does,
    /// once the daemon answers [`Control::ChildReleased`].
    ChildExited,
}

/// Daemon → wrapper messages during an execution.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Started,
    /// Stop without saving anything to resume. Older wrappers ignore the reason.
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Stop and save what `shoal resume` restores. Older wrappers ignore the
    /// reason.
    Pause {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The workspace is being removed; its removal record keeps the session.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        removed: bool,
    },
    OverloadStop {
        recover: bool,
        #[serde(default = "default_overload_reason")]
        reason: String,
        /// The condition automatic recovery waits for, completing "once …".
        #[serde(default = "default_resumes_when")]
        resumes_when: String,
    },
    Resume {
        ports: Vec<crate::model::PortReservation>,
    },
    Finished {
        complete: bool,
    },
    /// What the exited child left is stopped and the worktree is still the
    /// recorded one, so the wrapper may start its next run.
    ChildReleased,
    /// The daemon is shutting down; keep the command running and reattach.
    Detach,
}

fn default_overload_reason() -> String {
    "system overload".into()
}

/// What memory and CPU protection wait for, and what older daemons meant.
pub fn default_resumes_when() -> String {
    "system load is healthy".into()
}

pub async fn read<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    read_buffered(&mut BufReader::new(stream)).await
}

pub async fn read_buffered<T: DeserializeOwned>(
    stream: &mut (impl tokio::io::AsyncBufRead + Unpin),
) -> Result<T> {
    let mut bytes = Vec::new();
    stream
        .take(MAX_FRAME as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .await?;
    ensure!(
        bytes.len() <= MAX_FRAME,
        "protocol frame exceeds {MAX_FRAME} bytes"
    );
    // The peer closed the connection, possibly partway through a frame.
    if bytes.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "incomplete protocol frame",
        )
        .into());
    }
    serde_json::from_slice(&bytes).context("invalid protocol message")
}

/// `items` in pages that each fit in a frame with room for the envelope,
/// in order. There is always a first page, empty when `items` is.
pub fn frame_pages<T: Serialize>(items: Vec<T>) -> Result<Vec<Vec<T>>> {
    const PAGE_BYTES: usize = MAX_FRAME - 1024;
    let mut pages = vec![Vec::new()];
    let mut bytes = 0;
    for item in items {
        let size = serde_json::to_vec(&item)?.len() + 1;
        let page = pages.last_mut().expect("pages start with one");
        if bytes + size > PAGE_BYTES && !page.is_empty() {
            pages.push(Vec::new());
            bytes = 0;
        }
        bytes += size;
        pages.last_mut().expect("pages start with one").push(item);
    }
    Ok(pages)
}

/// `text` in pieces that each fit in a frame's page, however much JSON
/// escaping grows them.
pub fn text_pieces(text: &str) -> Vec<String> {
    const PIECE_BYTES: usize = MAX_FRAME / 4;
    let mut pieces = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(PIECE_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (piece, tail) = rest.split_at(end);
        pieces.push(piece.to_owned());
        rest = tail;
    }
    pieces
}

/// A message longer than [`MAX_FRAME`]; none of it was written.
#[derive(Debug)]
pub struct FrameTooLarge {
    pub bytes: usize,
}

impl std::fmt::Display for FrameTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the {}-byte message exceeds the {MAX_FRAME}-byte protocol frame",
            self.bytes
        )
    }
}

impl std::error::Error for FrameTooLarge {}

pub async fn write<T: Serialize>(stream: &mut (impl AsyncWrite + Unpin), value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_FRAME {
        return Err(FrameTooLarge { bytes: bytes.len() }.into());
    }
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn method_names_are_their_protocol_tags() {
        assert_eq!(Method::Status.name(), "status");
        let watch = Method::WatchNotifications {
            keep_unread: false,
            after: 0,
        };
        assert_eq!(watch.name(), "watch_notifications");
    }

    #[test]
    fn frame_pages_split_long_listings_in_order() {
        assert_eq!(
            frame_pages(Vec::<String>::new()).unwrap(),
            [Vec::<String>::new()]
        );
        let items: Vec<String> = (0..300).map(|n| format!("{n:0>500}")).collect();
        let pages = frame_pages(items.clone()).unwrap();
        assert!(pages.len() > 1);
        for page in &pages {
            assert!(serde_json::to_vec(page).unwrap().len() < MAX_FRAME - 512);
        }
        assert_eq!(pages.concat(), items);
    }

    #[test]
    fn text_pieces_fit_frames_and_keep_characters_whole() {
        assert!(text_pieces("").is_empty());
        // Quotes double when escaped; multibyte characters straddle piece ends.
        let text = format!("{}{}", "\"".repeat(MAX_FRAME), "é".repeat(MAX_FRAME));
        let pieces = text_pieces(&text);
        for page in frame_pages(pieces.clone()).unwrap() {
            assert!(serde_json::to_vec(&page).unwrap().len() < MAX_FRAME - 512);
        }
        assert_eq!(pieces.concat(), text);
    }

    #[tokio::test]
    async fn buffered_execution_reads_preserve_adjacent_frames() {
        let (mut writer, reader) = tokio::io::duplex(1024);
        write(&mut writer, &ExecutionEvent::Finished { exit_code: 7 })
            .await
            .unwrap();
        write(&mut writer, &ExecutionEvent::Finished { exit_code: 8 })
            .await
            .unwrap();
        let mut reader = BufReader::new(reader);
        for expected in [7, 8] {
            let event: ExecutionEvent = read_buffered(&mut reader).await.unwrap();
            assert!(
                matches!(event, ExecutionEvent::Finished { exit_code } if exit_code == expected)
            );
        }
    }

    #[tokio::test]
    async fn a_frame_cut_off_by_a_closed_connection_reads_as_end_of_stream() {
        for sent in [&b""[..], b"{\"type\":\"fin"] {
            let (mut writer, reader) = tokio::io::duplex(1024);
            writer.write_all(sent).await.unwrap();
            drop(writer);
            let error = read::<ExecutionEvent>(&mut BufReader::new(reader))
                .await
                .unwrap_err();
            let error = error.downcast_ref::<std::io::Error>().unwrap();
            assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
        }
    }

    #[test]
    fn daemon_error_codes_preserve_wire_spellings() {
        for (code, spelling) in [
            (ErrorCode::InvalidRequest, "invalid_request"),
            (ErrorCode::ProtocolMismatch, "protocol_mismatch"),
            (ErrorCode::ScopeDenied, "scope_denied"),
            (ErrorCode::ManagedService, "managed_service"),
            (ErrorCode::OperationFailed, "operation_failed"),
            (ErrorCode::ExecutionFailed, "execution_failed"),
            (ErrorCode::Unknown("future_code".into()), "future_code"),
        ] {
            let wire = json!({"protocol": VERSION, "id": 7, "type": "error",
                "data": {"code": spelling, "message": "failed"}});
            let response = Response::new(7, Body::error(code.clone(), "failed"));
            assert_eq!(serde_json::to_value(response).unwrap(), wire);
            let response: Response = serde_json::from_value(wire).unwrap();
            let error = response.body.into_result().unwrap_err();
            let remote = error.downcast_ref::<RemoteError>().unwrap();
            assert_eq!(remote.code, code);
            assert_eq!(remote.message, "failed");
            let display = match code {
                ErrorCode::OperationFailed => "failed".to_owned(),
                _ => format!("{spelling}: failed"),
            };
            assert_eq!(error.to_string(), display);
        }
        for code in [json!(null), json!(42), json!({"unknown": "future_code"})] {
            assert!(serde_json::from_value::<ErrorCode>(code).is_err());
        }
    }

    #[test]
    fn workspace_hook_rejects_unknown_kinds() {
        assert!(
            serde_json::from_value::<Method>(serde_json::json!({
                "workspace_hook": {"workspace": "worker", "kind": "unknown"}
            }))
            .is_err()
        );
    }

    #[test]
    fn pr_actions_preserve_existing_wire_fields() {
        use crate::daemon::links::pr::Action;
        for (action, url, clear) in [
            (
                Action::Watch {
                    url: "https://forge.example/team/repo/pulls/7".into(),
                },
                Some("https://forge.example/team/repo/pulls/7"),
                false,
            ),
            (Action::Acknowledge, None, false),
            (Action::Clear, None, true),
        ] {
            let wire = json!({"set_pr": {"workspace": "worker", "url": url, "clear": clear}});
            let decoded: Method = serde_json::from_value(wire.clone()).unwrap();
            let Method::SetPr {
                workspace,
                action: decoded_action,
            } = &decoded
            else {
                panic!("wrong method")
            };
            assert_eq!(workspace, "worker");
            assert_eq!(decoded_action, &action);
            assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
        }
        let decoded: Method =
            serde_json::from_value(json!({"set_pr": {"workspace": "worker", "clear": false}}))
                .unwrap();
        assert!(matches!(
            decoded,
            Method::SetPr {
                action: Action::Acknowledge,
                ..
            }
        ));
    }

    #[test]
    fn selective_unwatch_round_trips_and_rejects_conflicting_actions() {
        let wire =
            json!({"set_pr": {"workspace": "worker", "url": null, "clear": false, "unwatch": "7"}});
        let decoded: Method = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
        for action in [
            json!({"clear": true, "unwatch": "7"}),
            json!({"clear": false, "url": "8", "unwatch": "7"}),
        ] {
            assert!(serde_json::from_value::<crate::daemon::links::pr::Action>(action).is_err());
        }
    }

    #[test]
    fn pr_actions_reject_conflicting_wire_fields() {
        let error = serde_json::from_value::<Method>(
            json!({"set_pr": {"workspace": "worker", "url": "7", "clear": true}}),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("clear cannot include a URL"),
            "{error}"
        );
        assert!(
            serde_json::from_value::<Method>(
                json!({"set_pr": {"workspace": "worker", "url": "7"}})
            )
            .is_err()
        );
    }

    #[test]
    fn reattachment_keeps_its_wire_spelling() {
        let wire = json!({"reattach": {
            "execution": "execution", "wrapper": {"pid": 2, "birth": "wrapper"},
            "child": null, "group_id": 2, "scope_token": "token"
        }});
        let Method::Reattach(request) = serde_json::from_value(wire).unwrap() else {
            panic!("reattachment must keep its method name");
        };
        assert!(request.agent.is_none() && !request.recover);
        assert_eq!(
            serde_json::to_value(Control::Detach).unwrap(),
            json!({"type": "detach"})
        );
    }

    #[test]
    fn pause_reasons_are_optional_on_the_wire() {
        let control: Control = serde_json::from_value(json!({"type": "pause"})).unwrap();
        assert!(matches!(
            control,
            Control::Pause {
                reason: None,
                removed: false
            }
        ));
        let control: Control = serde_json::from_value(json!({"type": "stop"})).unwrap();
        assert!(matches!(control, Control::Stop { reason: None }));
        let wire = serde_json::to_value(Control::Pause {
            reason: Some("shoal stop".into()),
            removed: false,
        })
        .unwrap();
        assert_eq!(wire, json!({"type": "pause", "reason": "shoal stop"}));
        // Wrappers from before the reason read it as their unit variant.
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum Legacy {
            Pause,
        }
        assert!(matches!(
            serde_json::from_value::<Legacy>(wire).unwrap(),
            Legacy::Pause
        ));
    }

    #[test]
    fn older_overload_controls_use_a_generic_reason() {
        let control: Control =
            serde_json::from_value(json!({"type": "overload_stop", "recover": false})).unwrap();
        assert!(matches!(
            control,
            Control::OverloadStop { recover: false, reason, resumes_when }
                if reason == "system overload" && resumes_when == "system load is healthy"
        ));
    }

    #[test]
    fn tracked_execution_requests_require_a_known_kind() {
        let wrapper = crate::process::identity::capture(std::process::id())
            .unwrap()
            .unwrap();
        for (kind, spelling) in [
            (ExecutionKind::Command, "command"),
            (ExecutionKind::Setup, "setup"),
            (ExecutionKind::Land, "land"),
        ] {
            let method = Method::Execute {
                workspace: "worker".into(),
                wrapper: wrapper.clone(),
                kind,
                agent: Some("test-agent".into()),
                recover: false,
                reattach: true,
                queue_entry: None,
                demo: None,
            };
            let encoded = serde_json::to_value(method).unwrap();
            assert_eq!(encoded["execute"]["kind"], spelling);
            let Method::Execute {
                kind: decoded,
                agent,
                ..
            } = serde_json::from_value(encoded).unwrap()
            else {
                panic!("execution must use the shared request");
            };
            assert_eq!(decoded, kind);
            assert_eq!(agent.as_deref(), Some("test-agent"));
        }
        for kind in [None, Some("unknown"), Some("Setup")] {
            let mut request = json!({"execute": {"workspace": "worker", "wrapper": wrapper}});
            if let Some(kind) = kind {
                request["execute"]["kind"] = kind.into();
            }
            assert!(serde_json::from_value::<Method>(request).is_err());
        }
        for method in ["prepare", "land_workspace"] {
            assert!(
                serde_json::from_value::<Method>(json!({
                    method: {"workspace": "worker", "wrapper": wrapper}
                }))
                .is_err()
            );
        }
    }

    #[test]
    fn extraction_preserves_wire_shapes() {
        for (wire, expected) in [
            (json!({"type": "ok"}), "Ok"),
            (json!({"type": "workspaces", "data": []}), "Workspaces"),
            (
                json!({"type": "layered_config", "data": {"worktree_file": {}, "saved_repository_config": {}}}),
                "LayeredConfig",
            ),
            (json!({"type": "busy", "data": {"message": "busy"}}), "Busy"),
            (
                json!({"type": "error", "data": {"code": "future_code", "message": "failed"}}),
                "Error",
            ),
        ] {
            let body: Body = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(body.variant_name(), expected);
            // Config layers serialize defaults; other bodies round-trip exactly.
            if expected != "LayeredConfig" {
                assert_eq!(serde_json::to_value(&body).unwrap(), wire);
            }
            match expected {
                "Ok" => <()>::try_from(body).unwrap(),
                "Workspaces" => assert!(Vec::<Workspace>::try_from(body).unwrap().is_empty()),
                "LayeredConfig" => {
                    Box::<ConfigLayers>::try_from(body).unwrap();
                }
                "Error" => {
                    let error = DaemonStatus::try_from(body).unwrap_err();
                    let remote = error.downcast_ref::<RemoteError>().unwrap();
                    assert_eq!(remote.code, ErrorCode::Unknown("future_code".into()));
                    assert_eq!(remote.message, "failed");
                }
                _ => assert_eq!(
                    DaemonStatus::try_from(body).unwrap_err().to_string(),
                    format!("unexpected daemon response; expected Status, received {expected}")
                ),
            }
        }
    }

    #[test]
    fn stream_payloads_and_acknowledgements_preserve_remote_errors() {
        for error in [
            ExecutionPlan::try_from(Body::error(ErrorCode::ScopeDenied, "denied")).unwrap_err(),
            Notification::try_from(Body::error(ErrorCode::ScopeDenied, "denied")).unwrap_err(),
            <()>::try_from(Body::error(ErrorCode::ScopeDenied, "denied")).unwrap_err(),
        ] {
            let remote = error.downcast_ref::<RemoteError>().unwrap();
            assert_eq!(remote.code, ErrorCode::ScopeDenied);
            assert_eq!(remote.message, "denied");
        }
        let error = <()>::try_from(Body::Workspaces(vec![])).unwrap_err();
        assert_eq!(
            error.to_string(),
            "unexpected daemon response; expected Ok, received Workspaces"
        );
    }
}
