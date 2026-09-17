//! Newline-delimited JSON over the daemon's Unix socket. Bump [`VERSION`]
//! whenever a request, response, or event changes shape or spelling.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::{
    model::{
        DiffBase, ExecutionPlan, Inspection, PortOverview, PortReservation, PortSuggestion,
        PulledBranch, Repository, RepositoryRemoval, Workspace,
    },
    ports::PortRequest,
    process_identity::Identity,
    recovery::{ReconcileOptions, Report},
    removal::{BranchChoice, RemovalCheck, RemovalResult},
    repo_config::{Hooks, LocalConfig},
    resources::{Overview, ResourceLease, ResourceRequest},
    sim_audit::AuditEntry,
    simulators::{SimRequest, Simulator, SimulatorCatalog},
};

pub const VERSION: u32 = 18;
pub const MAX_FRAME: usize = 64 * 1024;

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
    RemoveRepository {
        repository: String,
    },
    // Workspaces.
    CreateWorkspace {
        repository: String,
        name: String,
        base: Option<String>,
    },
    ListWorkspaces,
    InspectWorkspace {
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
    Reconcile {
        workspace: Option<String>,
        options: ReconcileOptions,
    },
    PullDefaultBranch {
        workspace: String,
    },
    DiffBase {
        workspace: String,
    },
    /// The workspace's effective lifecycle hooks, resolved against its worktree.
    WorkspaceHooks {
        workspace: String,
    },
    /// Long-lived: the connection stays open for the execution's lifetime.
    Execute {
        workspace: String,
        wrapper: Identity,
    },
    /// Like [`Method::Execute`], running the configured setup command.
    Prepare {
        workspace: String,
        wrapper: Identity,
    },
    // Ports.
    ReservePort {
        workspace: String,
        name: String,
        request: PortRequest,
    },
    ReleasePort {
        workspace: String,
        name: String,
    },
    ListPorts {
        workspace: Option<String>,
    },
    PortOverview {
        workspace: String,
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
    ResourceList {
        workspace: Option<String>,
    },
    ResourceOverview {
        workspace: String,
    },
    // Simulators.
    SimCatalog,
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Body {
    Ok,
    Error { code: String, message: String },
    Status(Status),
    Repositories(Vec<Repository>),
    Repository(Repository),
    RepositoryConfig(LocalConfig),
    RepositoryRemoved(RepositoryRemoval),
    Workspace(Workspace),
    Workspaces(Vec<Workspace>),
    Inspection(Inspection),
    Reconciliation(Vec<Report>),
    Execution(ExecutionPlan),
    RemovalCheck(RemovalCheck),
    RemovalResult(RemovalResult),
    DiffBase(DiffBase),
    Hooks(Hooks),
    PulledBranch(PulledBranch),
    Port(PortReservation),
    Ports(Vec<PortReservation>),
    PortSuggestion(PortSuggestion),
    PortOverview(PortOverview),
    ResourceLease(ResourceLease),
    ResourceLeases(Vec<ResourceLease>),
    ResourceOverview(Overview),
    ResourceBusy { message: String },
    Simulator(Simulator),
    Simulators(Vec<Simulator>),
    SimBusy { message: String },
    SimCatalog(SimulatorCatalog),
    SimHistory(Vec<AuditEntry>),
}

impl Body {
    pub fn error(code: &str, error: impl std::fmt::Display) -> Self {
        Self::Error {
            code: code.into(),
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub version: String,
    pub uptime_secs: u64,
    pub managed: bool,
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
