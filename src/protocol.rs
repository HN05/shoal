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
        recovery::Report,
        resources::{Overview, ResourceLease},
    },
    model::{
        ConflictCheck, DiffBase, ExecutionPlan, Inspection, PortOverview, PortReservation,
        PortSuggestion, Repository, RepositoryRemoval, SyncedRepository, Workspace,
        WorkspaceStatus,
    },
    process::identity::Identity,
    removal::{RemovalCheck, RemovalResult},
    sim::{Simulator, SimulatorCatalog, audit::AuditEntry},
};

mod agents;
mod approvals;
mod assignments;
mod chats;
mod ci;
mod daemon;
mod demos;
mod executions;
mod files;
mod holds;
mod items;
mod notifications;
mod ports;
mod questions;
mod queue;
mod reads;
mod recovery;
mod removal;
mod repositories;
mod resources;
mod secrets;
mod simulators;
mod workspaces;

pub use agents::{AgentMethod, TurnEnd, WokenTurn};
pub use approvals::ApprovalMethod;
pub use assignments::AssignmentMethod;
pub use chats::ChatMethod;
pub use ci::CiMethod;
pub use daemon::{DaemonMethod, Exit, ExitRequest};
pub use demos::DemoMethod;
pub use executions::ExecutionMethod;
pub use files::FileMethod;
pub use holds::HoldMethod;
pub use items::{ItemContext, ItemMethod};
pub use notifications::NotificationMethod;
pub use ports::PortMethod;
pub use questions::QuestionMethod;
pub use queue::QueueMethod;
pub use recovery::RecoveryMethod;
pub use removal::RemovalMethod;
pub use repositories::RepositoryMethod;
pub use resources::ResourceMethod;
pub use secrets::SecretMethod;
pub use simulators::SimulatorMethod;
pub use workspaces::WorkspaceMethod;

pub const VERSION: u32 = 161;
pub const MAX_FRAME: usize = 64 * 1024;

/// Shared CLI, daemon, and wrapper timing; keep related budgets in view when tuning.
pub mod timing {
    use std::time::Duration;

    /// Release a daemon connection slot if the client never sends its request.
    pub const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
    /// Status and shutdown should fail promptly when the daemon is unresponsive.
    pub const ADMIN_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
    /// A status request during daemon startup waits this long for it to end
    /// before learning the running step, within the admin request timeout.
    pub const STARTUP_STATUS_WAIT: Duration = Duration::from_secs(2);
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
    /// Wait within one scan for processes caught mid-exec to publish their
    /// environment, and again for the ones an inspection finds unattributed.
    pub const PROCESS_SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
    /// Cover ordinary EXECUTION_START_TIMEOUT plus START_ACK_TIMEOUT and wrapper startup.
    pub const DETACHED_LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);
    /// Collect a failed detached wrapper's exit status without waiting indefinitely.
    pub const DETACHED_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
    /// Give the child time to handle a stop signal before escalating to SIGKILL.
    pub const EXECUTION_STOP_GRACE: Duration = Duration::from_secs(2);
    /// Retry incomplete process proof without extending the wrapper completion budget.
    pub const PROCESS_PROOF_TIMEOUT: Duration = Duration::from_secs(10);
    /// Cover child registration, its stop grace, and the daemon completion acknowledgement.
    pub const WORKSPACE_STOP_TIMEOUT: Duration = Duration::from_secs(90);
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
    pub const REATTACH_EXIT_TIMEOUT: Duration = Duration::from_secs(120);

    const _: () = {
        // Each pass scans markers, then inspects related processes with a
        // second inventory and lets unattributed ones settle; an exited agent
        // gets a stop pass before completion.
        assert!(
            COMPLETION_ACK_TIMEOUT.as_millis()
                > 2 * (2 * PROCESS_INVENTORY_TIMEOUT.as_millis()
                    + 2 * PROCESS_SETTLE_TIMEOUT.as_millis())
                    + PROCESS_TERM_GRACE.as_millis()
                    + PROCESS_KILL_WAIT.as_millis()
        );
        assert!(
            DETACHED_LAUNCH_TIMEOUT.as_millis()
                > EXECUTION_START_TIMEOUT.as_millis() + START_ACK_TIMEOUT.as_millis()
        );
        assert!(
            WORKSPACE_STOP_TIMEOUT.as_millis()
                > START_ACK_TIMEOUT.as_millis()
                    + EXECUTION_STOP_GRACE.as_millis()
                    + COMPLETION_ACK_TIMEOUT.as_millis()
        );
        assert!(PROCESS_PROOF_TIMEOUT.as_millis() < COMPLETION_ACK_TIMEOUT.as_millis());
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
    pub fn new(method: impl Into<Method>) -> Self {
        Self {
            protocol: VERSION,
            id: 1,
            method: method.into(),
            scope: crate::env::scope_token(),
        }
    }
}

/// Every request; each decodes from its own spelling.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Method {
    /// Answered before the protocol check and authorization.
    Reattach(Reattachment),
    /// Answered before the protocol check.
    Exit(ExitRequest),
    Operation(Operation),
}

/// A request that resumes a connection rather than asking a domain.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reattachment {
    /// Long-lived: an execution connection resumed after a daemon restart.
    Reattach(Reattach),
}

/// A request one of the daemon's domains answers, by its domain.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Operation {
    Daemon(DaemonMethod),
    Repository(RepositoryMethod),
    Workspace(WorkspaceMethod),
    Removal(RemovalMethod),
    Recovery(RecoveryMethod),
    Execution(ExecutionMethod),
    Assignment(AssignmentMethod),
    Agent(AgentMethod),
    Chat(ChatMethod),
    Queue(QueueMethod),
    Hold(HoldMethod),
    Demo(DemoMethod),
    File(FileMethod),
    Item(ItemMethod),
    Ci(CiMethod),
    Notification(NotificationMethod),
    Question(QuestionMethod),
    Approval(ApprovalMethod),
    Port(PortMethod),
    Secret(SecretMethod),
    Resource(ResourceMethod),
    Simulator(SimulatorMethod),
}

impl Method {
    /// The request's protocol name, for timing lines.
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
        match self {
            Self::Reattach(_) => false,
            Self::Exit(_) => true,
            Self::Operation(operation) => operation.controls_daemon(),
        }
    }

    /// How long the request keeps its connection.
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::Reattach(_) => Lifetime::Resumed,
            Self::Exit(_) => Lifetime::Reply,
            Self::Operation(operation) => operation.lifetime(),
        }
    }

    /// Executions, waits and streams, which keep the connection open after
    /// the request.
    pub fn holds_connection(&self) -> bool {
        self.lifetime() != Lifetime::Reply
    }

    /// Long-lived requests whose clients send them again when the daemon
    /// closes the connection unanswered, as across a restart.
    pub fn retries_when_closed(&self) -> bool {
        self.lifetime() == Lifetime::Resumed
    }
}

impl Operation {
    /// Daemon administration, which must reach a daemon of any release.
    pub fn controls_daemon(&self) -> bool {
        matches!(self, Self::Daemon(_))
    }

    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::Workspace(method) => method.lifetime(),
            Self::Execution(method) => method.lifetime(),
            Self::Agent(method) => method.lifetime(),
            Self::Chat(method) => method.lifetime(),
            Self::Queue(method) => method.lifetime(),
            Self::Item(method) => method.lifetime(),
            Self::Notification(method) => method.lifetime(),
            Self::Question(method) => method.lifetime(),
            Self::Daemon(_)
            | Self::Repository(_)
            | Self::Removal(_)
            | Self::Recovery(_)
            | Self::Assignment(_)
            | Self::Hold(_)
            | Self::Demo(_)
            | Self::File(_)
            | Self::Ci(_)
            | Self::Approval(_)
            | Self::Port(_)
            | Self::Secret(_)
            | Self::Resource(_)
            | Self::Simulator(_) => Lifetime::Reply,
        }
    }
}

impl From<Exit> for Method {
    fn from(exit: Exit) -> Self {
        Self::Exit(ExitRequest { exit })
    }
}

impl From<Operation> for Method {
    fn from(operation: Operation) -> Self {
        Self::Operation(operation)
    }
}

/// Each domain's requests convert into the operation and call carrying them.
macro_rules! from_domains {
    ($($variant:ident($method:ty)),+ $(,)?) => {$(
        impl From<$method> for Operation {
            fn from(method: $method) -> Self {
                Self::$variant(method)
            }
        }

        impl From<$method> for Method {
            fn from(method: $method) -> Self {
                Self::Operation(method.into())
            }
        }
    )+};
}

from_domains!(
    Daemon(DaemonMethod),
    Repository(RepositoryMethod),
    Workspace(WorkspaceMethod),
    Removal(RemovalMethod),
    Recovery(RecoveryMethod),
    Execution(ExecutionMethod),
    Assignment(AssignmentMethod),
    Agent(AgentMethod),
    Chat(ChatMethod),
    Queue(QueueMethod),
    Hold(HoldMethod),
    Demo(DemoMethod),
    File(FileMethod),
    Item(ItemMethod),
    Ci(CiMethod),
    Notification(NotificationMethod),
    Question(QuestionMethod),
    Approval(ApprovalMethod),
    Port(PortMethod),
    Secret(SecretMethod),
    Resource(ResourceMethod),
    Simulator(SimulatorMethod),
);

/// How long a request keeps its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifetime {
    /// Until the reply.
    Reply,
    /// Until either side hangs up, as an execution or a stream does.
    Open,
    /// Until either side hangs up; the client sends it again when the daemon
    /// closes the connection unanswered, as across a restart.
    Resumed,
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
        OperationFailed => "operation_failed",
        ExecutionFailed => "execution_failed",
        /// Stopping needs the user to confirm that an execution Shoal cannot
        /// prove ended did.
        UnprovenStop => "unproven_stop",
        /// A restart continues once the stopped agent's remaining processes
        /// exit, which the message names.
        RestartWaits => "restart_waits",
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
    RepositoryMoves(crate::model::RepositoryMoves),
    MovedRepositories(Vec<crate::model::MovedRepository>),
    Workspace(Workspace),
    Hold(crate::model::WorkspaceHold),
    QueueAdmission(crate::model::QueueAdmission),
    QueueEntries(Vec<crate::model::QueueEntry>),
    RunningAgents(Vec<crate::model::RunningAgent>),
    OwnedPlaces(Vec<crate::model::OwnedPlace>),
    PlaceClose(crate::sessions::PlaceClose),
    PlaceOwnership(crate::sessions::PlaceOwnership),
    QueueEntry(crate::model::QueueEntry),
    MovedEntry(crate::model::MovedEntry),
    QueuedPlan(crate::model::QueuedPlan),
    Holds(Vec<crate::model::WorkspaceHold>),
    AttachedFiles(Vec<crate::daemon::workspace_files::AttachedFile>),
    Demos(Vec<crate::demos::Running>),
    DemoProgress(crate::demos::Progress),
    Workspaces(Vec<Workspace>),
    Chat(crate::model::Chat),
    Chats(Vec<crate::model::Chat>),
    ChatName(String),
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
    WokenTurn(WokenTurn),
    AgentSession(crate::model::AgentSession),
    WorkspaceStatus(WorkspaceStatus),
    SelectedItems(crate::model::Selected),
    MergedPull(crate::forge::items::revert::MergedPull),
    CheckLog(crate::forge::ci::checks::CheckLog),
    CiActed(crate::forge::ci::CiActed),
    CiRunStarted(crate::forge::ci::run::StartedRun),
    CiRunActed(crate::forge::ci::run_action::RunActed),
    CiRun(crate::forge::ci::run_action::Run),
    CiRunJobs(Vec<crate::forge::ci::run_view::RunJob>),
    CiRuns(Vec<crate::forge::ci::run_action::Run>),
    WorkspaceEnv(std::collections::BTreeMap<String, String>),
    Cleanup(crate::daemon::cleanup::ManualCleanup),
    RemovedWorkspaces(Vec<crate::model::RemovedWorkspace>),
    AgentEnvironments(Vec<std::collections::BTreeMap<String, String>>),
    KeptBranches(Vec<crate::model::KeptBranch>),
    UntrackedWorktrees(Vec<crate::model::UntrackedWorktree>),
    RestoredWorkspace(crate::model::RestoredWorkspace),
    Checkpoint(crate::model::Checkpoint),
    Checkpoints(Option<Vec<crate::model::Checkpoint>>),
    RestoredCheckpoint(crate::model::RestoredCheckpoint),
    FastForwarded(bool),
    ForgeType(crate::forge::backends::ForgeKind),
    Reconciled(Vec<Report>),
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
    SecretCatalog(crate::daemon::secrets::SecretCatalog),
    ResourceLease(ResourceLease),
    ResourceOverview(Overview),
    ResourceCatalog(crate::daemon::resources::Catalog),
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

    /// `shoal restart`, whose wrapper restores the agent once its processes
    /// stop. Wrappers handle it as any protection stop they restore.
    pub(crate) fn restart() -> Self {
        Self {
            reason: RESTART_REASON.into(),
            resumes_when: "its processes stop".into(),
        }
    }

    pub(crate) fn is_restart(&self) -> bool {
        self.reason == RESTART_REASON
    }

    /// `shoal queue move` of a running agent, whose wrapper waits for the
    /// agent queue to start its restore. Wrappers handle it as any
    /// protection stop they restore.
    pub(crate) fn queue_move() -> Self {
        Self {
            reason: MOVE_REASON.into(),
            resumes_when: "the agent queue starts it".into(),
        }
    }

    pub(crate) fn is_queue_move(&self) -> bool {
        self.reason == MOVE_REASON
    }

    /// An idle agent of a done workspace whose cleanup is held, which Shoal
    /// stops to free memory.
    pub(crate) fn idle_held() -> Self {
        Self::idle(IDLE_HELD_REASON)
    }

    /// An idle agent whose prompt cache expired, which Shoal stops to free
    /// memory under `[idle_stop]`.
    pub(crate) fn idle_expired() -> Self {
        Self::idle(IDLE_EXPIRED_REASON)
    }

    /// An idle stop for `reason`, whose restore waits for an update. Wrappers
    /// handle it as any protection stop they restore.
    fn idle(reason: &str) -> Self {
        Self {
            reason: reason.into(),
            resumes_when: "an update arrives for it".into(),
        }
    }

    /// Whether Shoal stopped the agent for being idle, not for load.
    pub(crate) fn is_idle(&self) -> bool {
        [IDLE_HELD_REASON, IDLE_EXPIRED_REASON].contains(&self.reason.as_str())
    }

    /// Whether `[overload.recovery]` decides its restore, as for a stop that
    /// load or disk space caused; `shoal queue move` and idle stops restore
    /// whatever it says.
    pub(crate) fn follows_overload_recovery(&self) -> bool {
        !self.is_queue_move() && !self.is_idle()
    }

    /// Whether a done assignment cancels its restore. An idle stop's restore
    /// waits for an update all the same, since the agent ran until Shoal
    /// stopped it.
    pub(crate) fn ends_with_assignment(&self) -> bool {
        !self.is_idle()
    }
}

/// The stop reason a restarted agent's wrapper shows and saves.
const RESTART_REASON: &str = "restarted by shoal restart";

/// The stop reason of an agent `shoal queue move` sent to the queue.
const MOVE_REASON: &str = "moved in the agent queue by shoal queue move";

/// The stop reason of an idle agent of a done workspace whose cleanup is held.
const IDLE_HELD_REASON: &str = "idle in a done workspace whose cleanup is held";

/// The stop reason of an idle agent `[idle_stop]` stops.
const IDLE_EXPIRED_REASON: &str = "idle with its prompt cache expired";

/// Wrapper → daemon messages after an [`ExecutionMethod::Execute`] response.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionEvent {
    Started {
        child: Option<Identity>,
        group_id: u32,
        /// The child restores a saved agent, which leaves the workspace's
        /// other last sessions; a new agent replaces them.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        restored: bool,
    },
    Finished {
        exit_code: i32,
    },
    Paused,
    /// The child exited and the wrapper starts another, as a demo's does,
    /// once the daemon answers [`Control::ChildReleased`].
    ChildExited,
    /// The wrapper continues from the installed binary, which an upgrade put
    /// in place of its own, between a [`Control::Resume`] that allows it and
    /// its next child's start, and registers again from `origin`, the place it runs in. The
    /// daemon answers [`Control::Registered`], or a stop.
    Register {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Box<crate::sessions::Origin>>,
    },
}

/// Daemon → wrapper messages during an execution.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Started,
    /// Stop without saving anything to resume. Older wrappers ignore the
    /// reason and `removed`.
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The workspace is being removed; its removal record keeps the agent
        /// whose restore this cancels.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        removed: bool,
    },
    /// Stop and save what `shoal resume` restores. Older wrappers ignore the
    /// reason.
    Pause {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The workspace is being removed; its removal record keeps the session.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        removed: bool,
        /// The system is shutting down, which may end the wrapper before its
        /// command: save the session first, as a signal ending it does.
        /// Older wrappers pause as `shoal stop` does.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        shutdown: bool,
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
        /// The binary the daemon runs, which a wrapper whose own binary an
        /// upgrade replaced may continue from, since it speaks the daemon's
        /// protocol. Older daemons leave it out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        executable: Option<crate::fsutil::FileIdentity>,
    },
    Finished {
        complete: bool,
    },
    /// What the exited child left is stopped and the worktree is still the
    /// recorded one, so the wrapper may start its next run.
    ChildReleased,
    /// The daemon is shutting down; keep the command running and reattach.
    Detach,
    /// The plan of an execution whose wrapper registered again.
    Registered {
        plan: Box<crate::model::ExecutionPlan>,
    },
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
mod tests;
