//! Newline-delimited JSON over the daemon's Unix socket. Bump [`VERSION`]
//! whenever a request, response, or event changes shape or spelling.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::{
    config::{
        named_commands::CommandLayers,
        repo::{ConfigLayers, LocalConfig},
    },
    daemon::{
        allocation::Allocation,
        notifications::Notification,
        ports::PortRequest,
        recovery::{ReconcileOptions, Report},
        resources::{Overview, ResourceLease, ResourceRequest},
        workspace::ExecutionKind,
    },
    hooks::HookKind,
    model::{
        DiffBase, ExecutionPlan, Inspection, PortOverview, PortReservation, PortSuggestion,
        PulledBranch, Repository, RepositoryRemoval, Workspace, WorkspaceStatus,
    },
    process::identity::Identity,
    removal::{BranchChoice, RemovalCheck, RemovalResult},
    sim::{SimRequest, Simulator, SimulatorCatalog, audit::AuditEntry},
};

pub const VERSION: u32 = 41;
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

    /// Ordinary command registration needs no slow preparation.
    pub const EXECUTION_START_TIMEOUT: Duration = Duration::from_secs(5);
    /// Setup and landing can wait for Git gates, hooks, and upstream refreshes.
    pub const PREPARED_EXECUTION_START_TIMEOUT: Duration = Duration::from_secs(120);
    /// Wait for the daemon to persist the spawned child's process group.
    pub const START_ACK_TIMEOUT: Duration = Duration::from_secs(10);
    /// Leave room beyond PROCESS_INVENTORY_TIMEOUT for ownership checks and persistence.
    pub const COMPLETION_ACK_TIMEOUT: Duration = Duration::from_secs(20);
    /// Bound the ps inventory subprocess used by the daemon's completion scan.
    pub const PROCESS_INVENTORY_TIMEOUT: Duration = Duration::from_secs(10);
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

    const _: () = {
        assert!(COMPLETION_ACK_TIMEOUT.as_millis() > PROCESS_INVENTORY_TIMEOUT.as_millis());
        assert!(
            DETACHED_LAUNCH_TIMEOUT.as_millis()
                > EXECUTION_START_TIMEOUT.as_millis() + START_ACK_TIMEOUT.as_millis()
        );
        assert!(WORKSPACE_STOP_TIMEOUT.as_millis() > EXECUTION_STOP_GRACE.as_millis());
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
    // Repositories.
    ListRepositories,
    RegisterRepository {
        source: String,
        name: Option<String>,
        path: Option<std::path::PathBuf>,
    },
    RenameRepository {
        repository: String,
        name: String,
    },
    RepositoryConfig {
        repository: String,
    },
    SetRepositoryConfig {
        repository: String,
        toml: Option<String>,
    },
    EditRepositoryConfig {
        repository: String,
        key: String,
        value: Option<String>,
    },
    RemoveRepository {
        repository: String,
    },
    // Workspaces.
    ListBranches {
        repository: String,
    },
    OpenBranch {
        path: Option<std::path::PathBuf>,
        repository: String,
        branch: String,
        git_profile: Option<String>,
        base: Option<String>,
    },
    CreateWorkspace {
        path: Option<std::path::PathBuf>,
        repository: String,
        name: String,
        base: Option<String>,
        git_profile: Option<String>,
    },
    AdoptWorkspace {
        repository: String,
        path: std::path::PathBuf,
    },
    ListWorkspaces,
    SetPr {
        workspace: String,
        url: Option<String>,
        clear: bool,
    },
    InspectWorkspace {
        workspace: String,
    },
    WorkspaceStatus {
        workspace: String,
    },
    StopWorkspace {
        workspace: String,
    },
    CheckRemoval {
        workspace: String,
        caller_pid: u32,
    },
    RemoveWorkspace {
        workspace: String,
        choice: BranchChoice,
        caller_pid: u32,
    },
    Diagnose,
    Doctor {
        workspace: Option<String>,
        options: ReconcileOptions,
    },
    /// Verify that this worker belongs to an unscoped caller's landing execution.
    CheckLanding,
    /// Fast-forward a local merge source from its upstream before merging.
    RefreshMergeSource {
        workspace: String,
        branch: String,
    },
    DiffBase {
        workspace: String,
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
    /// The separate repository command maps needed to report provenance.
    CommandLayers {
        workspace: String,
    },
    /// Long-lived: the connection stays open for the execution's lifetime.
    /// `agent` names a Shoal agent shortcut whose exit the user is told about.
    Execute {
        workspace: String,
        wrapper: Identity,
        kind: ExecutionKind,
        #[serde(default)]
        agent: Option<String>,
    },
    // Notifications.
    ListNotifications {
        unread_only: bool,
        limit: u32,
    },
    MarkNotificationsRead {
        ids: Vec<i64>,
    },
    /// Long-lived: unread notifications, then new ones as they are recorded,
    /// each as a [`Body::Notification`] response and marked read on delivery.
    WatchNotifications,
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
    Repository(String),
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

// Keep payload conversions and variant names tied to the wire enum.
macro_rules! response_bodies {
    ($($variant:ident($payload:ty),)*) => {
        #[derive(Debug, Serialize, Deserialize)]
        #[serde(tag = "type", content = "data", rename_all = "snake_case")]
        pub enum Body {
            Ok,
            Error { code: String, message: String },
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
    Workspaces(Vec<Workspace>),
    Branches(Vec<crate::git::existing_branch::Branch>),
    OpenedWorkspace(crate::git::existing_branch::OpenedWorkspace),
    Inspection(Inspection),
    WorkspaceStatus(WorkspaceStatus),
    Diagnostics(Vec<crate::daemon::doctor::Check>),
    Doctor(Vec<Report>),
    Execution(ExecutionPlan),
    RemovalCheck(RemovalCheck),
    RemovalResult(RemovalResult),
    DiffBase(DiffBase),
    Hook(Option<std::path::PathBuf>),
    LayeredConfig(Box<ConfigLayers>),
    CommandLayers(CommandLayers),
    PulledBranch(PulledBranch),
    Notifications(Vec<Notification>),
    Notification(Notification),
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
    pub code: String,
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
        match self {
            Self::Error { code, message } => RemoteError { code, message }.into(),
            body => anyhow::anyhow!(
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

    pub fn error(code: &str, error: impl std::fmt::Display) -> Self {
        Self::Error {
            code: code.into(),
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
}

/// Daemon → wrapper messages during an execution.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Started,
    Stop,
    Finished { complete: bool },
}

pub async fn read<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    let mut bytes = Vec::new();
    BufReader::new(stream.take(MAX_FRAME as u64 + 1))
        .read_until(b'\n', &mut bytes)
        .await?;
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
                    assert_eq!(remote.code, "future_code");
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
            ExecutionPlan::try_from(Body::error("scope_denied", "denied")).unwrap_err(),
            Notification::try_from(Body::error("scope_denied", "denied")).unwrap_err(),
            <()>::try_from(Body::error("scope_denied", "denied")).unwrap_err(),
        ] {
            let remote = error.downcast_ref::<RemoteError>().unwrap();
            assert_eq!(remote.code, "scope_denied");
            assert_eq!(remote.message, "denied");
        }
        let error = <()>::try_from(Body::Workspaces(vec![])).unwrap_err();
        assert_eq!(
            error.to_string(),
            "unexpected daemon response; expected Ok, received Workspaces"
        );
    }
}
