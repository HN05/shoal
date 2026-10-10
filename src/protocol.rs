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
    removal::{BranchChoice, RemovalCheck, RemovalResult},
    sim::{SimRequest, Simulator, SimulatorCatalog, audit::AuditEntry},
};

pub const VERSION: u32 = 77;
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
    /// Observe daemon transitions promptly without busy-polling the socket.
    pub const DAEMON_POLL_INTERVAL: Duration = Duration::from_millis(100);

    /// Ordinary command registration needs no slow preparation, but its process
    /// and worktree checks run subprocesses; only guard against a hung daemon.
    pub const EXECUTION_START_TIMEOUT: Duration = Duration::from_secs(30);
    /// Setup and landing can wait for Git gates, hooks, and upstream refreshes.
    pub const PREPARED_EXECUTION_START_TIMEOUT: Duration = Duration::from_secs(120);
    /// Wait for the daemon to persist the spawned child's process group.
    pub const START_ACK_TIMEOUT: Duration = Duration::from_secs(10);
    /// Leave room beyond the process scan budgets for ownership checks and persistence.
    pub const COMPLETION_ACK_TIMEOUT: Duration = Duration::from_secs(20);
    /// Bound the ps inventory subprocess used by the daemon's completion scan.
    pub const PROCESS_INVENTORY_TIMEOUT: Duration = Duration::from_secs(10);
    /// Wait within one scan for processes caught mid-exec to publish their environment.
    pub const PROCESS_SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
    /// Cover ordinary EXECUTION_START_TIMEOUT plus START_ACK_TIMEOUT and wrapper startup.
    pub const DETACHED_LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);
    /// Collect a failed detached wrapper's exit status without waiting indefinitely.
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
        assert!(
            COMPLETION_ACK_TIMEOUT.as_millis()
                > PROCESS_INVENTORY_TIMEOUT.as_millis() + PROCESS_SETTLE_TIMEOUT.as_millis()
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
        source: crate::forge::repository::Selector,
        name: Option<String>,
        path: Option<std::path::PathBuf>,
    },
    RenameRepository {
        repository: crate::forge::repository::Selector,
        name: String,
    },
    RepositoryConfig {
        repository: crate::forge::repository::Selector,
    },
    SetRepositoryConfig {
        repository: crate::forge::repository::Selector,
        toml: Option<String>,
    },
    EditRepositoryConfig {
        repository: crate::forge::repository::Selector,
        changes: Vec<crate::config::edit::Change>,
    },
    RemoveRepository {
        repository: crate::forge::repository::Selector,
    },
    /// Fetch the default branch's remote and fast-forward the local default branch.
    SyncRepository {
        repository: crate::forge::repository::Selector,
    },
    // Workspaces.
    ListBranches {
        repository: crate::forge::repository::Selector,
    },
    OpenBranch {
        path: Option<std::path::PathBuf>,
        repository: crate::forge::repository::Selector,
        branch: String,
        git_profile: Option<String>,
        base: Option<String>,
    },
    CreateWorkspace {
        path: Option<std::path::PathBuf>,
        repository: crate::forge::repository::Selector,
        name: String,
        base: Option<String>,
        git_profile: Option<String>,
    },
    AdoptWorkspace {
        repository: crate::forge::repository::Selector,
        path: std::path::PathBuf,
        copy: bool,
    },
    RenameWorkspace {
        workspace: String,
        branch: String,
    },
    ListWorkspaces,
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
    /// Record or clear the workspace whose branch this one builds on.
    SetBaseWorkspace {
        workspace: String,
        base: Option<String>,
    },
    WorkspaceUndone {
        workspace: String,
    },
    WorkspaceDone {
        workspace: String,
        cleanup: Option<bool>,
    },
    MarkReady {
        workspace: String,
        selection: crate::forge::link::Selection,
    },
    ClearReady {
        workspace: String,
        selection: crate::forge::link::Selection,
    },
    /// The caller's execution, when scoped, is recorded as the reporter.
    SetAgentState {
        workspace: String,
        state: crate::state::AgentState,
    },
    SetIssue {
        workspace: String,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    ClearIssue {
        workspace: String,
        url: Option<String>,
    },
    SetPr {
        workspace: String,
        #[serde(flatten)]
        action: crate::forge::pr::Action,
    },
    PrWait {
        workspace: String,
        timeout_secs: u64,
    },
    WatchItems {
        workspace: String,
        selection: crate::forge::link::Selection,
        timeout_secs: u64,
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
    /// Link the open PR of the workspace's branch, or open one and link it.
    OpenPull {
        workspace: String,
        options: crate::forge::create::PullOptions,
    },
    /// Open an issue in the workspace's repository, linking it when `link` is set.
    OpenIssue {
        workspace: String,
        issue: crate::forge::create::NewIssue,
        link: bool,
    },
    /// Change an issue or PR of the workspace's repository: the one `item`
    /// names, or the workspace's linked one of `kind`.
    ItemAction {
        workspace: String,
        kind: crate::forge::link::ItemKind,
        item: Option<String>,
        action: crate::forge::action::Action,
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
    /// Linked items, or an explicit one, for the CLI to look up.
    SelectItems {
        workspace: String,
        selection: crate::forge::link::Selection,
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
        caller_pid: u32,
        #[serde(default)]
        include_changes: bool,
    },
    RemoveWorkspace {
        workspace: String,
        choice: BranchChoice,
        caller_pid: u32,
    },
    /// Remove idle cleanup candidates without waiting for their idle delay.
    Cleanup {
        dry_run: bool,
    },
    Diagnose,
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
    /// Long-lived: unread notifications, then new ones as they are recorded,
    /// each as a [`Body::Notification`] response and marked read on delivery.
    WatchNotifications,
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
    ListAccess {
        workspace: Option<String>,
    },
    DecideAccess {
        id: String,
        approve: bool,
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
    Repository(crate::forge::repository::Selector),
}

impl<T> Allocation<T> {
    pub fn into_body(self, granted: impl FnOnce(T) -> Body) -> Body {
        match self {
            Self::Granted(value) => granted(value),
            Self::Busy(message) => Body::Busy { message },
            Self::Approval(request) => Body::AccessRequest(request),
        }
    }
}

// Error codes are open on receipt so newer daemons can still explain failures.
// Keep the known vocabulary and its wire/display spellings in one place.
macro_rules! error_codes {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(from = "String")]
        pub enum ErrorCode {
            $(#[serde(rename = $wire)] $variant,)+
            /// An unfamiliar daemon code, preserved verbatim for compatibility.
            #[serde(untagged)]
            Unknown(String),
        }

        impl From<String> for ErrorCode {
            fn from(value: String) -> Self {
                match value.as_str() {
                    $($wire => Self::$variant,)+
                    _ => Self::Unknown(value),
                }
            }
        }

        impl std::fmt::Display for ErrorCode {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(match self {
                    $(Self::$variant => $wire,)+
                    Self::Unknown(value) => value,
                })
            }
        }
    };
}

error_codes! {
    InvalidRequest => "invalid_request",
    ProtocolMismatch => "protocol_mismatch",
    ScopeDenied => "scope_denied",
    ManagedService => "managed_service",
    OperationFailed => "operation_failed",
    ExecutionFailed => "execution_failed",
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
    Workspace(Workspace),
    Hold(crate::model::WorkspaceHold),
    Holds(Vec<crate::model::WorkspaceHold>),
    Workspaces(Vec<Workspace>),
    Branches(Vec<crate::git::existing_branch::Branch>),
    OpenedWorkspace(crate::git::existing_branch::OpenedWorkspace),
    Inspection(Inspection),
    Item(crate::forge::item::Item),
    Opened(crate::forge::item::Opened),
    Completion(crate::model::Completion),
    WithdrawnCompletion(Option<crate::model::Completion>),
    ReviewMarks(Vec<crate::model::ReviewMark>),
    AgentStatus(crate::model::AgentStatus),
    WorkspaceStatus(WorkspaceStatus),
    SelectedItems(crate::forge::view::Selected),
    WorkspaceEnv(std::collections::BTreeMap<String, String>),
    Cleanup(crate::daemon::cleanup::ManualCleanup),
    Diagnostics(Vec<crate::daemon::doctor::Check>),
    Doctor(Vec<Report>),
    Execution(ExecutionPlan),
    RemovalCheck(RemovalCheck),
    RemovalResult(RemovalResult),
    DiffBase(DiffBase),
    Conflicts(ConflictCheck),
    Hook(Option<std::path::PathBuf>),
    LayeredConfig(Box<ConfigLayers>),
    SyncedRepository(SyncedRepository),
    Notifications(Vec<Notification>),
    AgentMessages(Vec<crate::daemon::agent_messages::AgentMessage>),
    Notification(Notification),
    EventItem(EventItem),
    PrUpdates(crate::forge::pr::wait::Updates),
    Port(PortReservation),
    PortSuggestion(PortSuggestion),
    PortOverview(PortOverview),
    AccessRequest(Box<crate::daemon::access::AccessRequest>),
    AccessRequests(Vec<crate::daemon::access::AccessRequest>),
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

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
    if bytes.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "incomplete protocol frame",
        )
        .into());
    }
    ensure!(
        bytes.len() <= MAX_FRAME,
        "protocol frame exceeds {MAX_FRAME} bytes"
    );
    ensure!(bytes.last() == Some(&b'\n'), "incomplete protocol frame");
    serde_json::from_slice(&bytes).context("invalid protocol message")
}

pub async fn write<T: Serialize>(stream: &mut (impl AsyncWrite + Unpin), value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    ensure!(bytes.len() <= MAX_FRAME, "protocol response is too large");
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
            assert_eq!(error.to_string(), format!("{spelling}: failed"));
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
        use crate::forge::pr::Action;
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
            assert!(serde_json::from_value::<crate::forge::pr::Action>(action).is_err());
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
        assert!(matches!(control, Control::Pause { reason: None }));
        let control: Control = serde_json::from_value(json!({"type": "stop"})).unwrap();
        assert!(matches!(control, Control::Stop { reason: None }));
        let wire = serde_json::to_value(Control::Pause {
            reason: Some("shoal stop".into()),
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
