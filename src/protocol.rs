use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::model::{ExecutionPlan, Inspection, Repository, Workspace};

pub const VERSION: u32 = 8;
pub const MAX_FRAME: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub id: u64,
    pub method: Method,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    ResourceAcquire {
        workspace: String,
        request: crate::resources::AcquireRequest,
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
    SimCatalog,
    SimHistory {
        workspace: Option<String>,
        limit: u32,
        before: Option<i64>,
    },
    SimList {
        workspace: Option<String>,
    },
    SimAcquire {
        workspace: String,
        request: crate::simulators::SimRequest,
    },
    SimRelease {
        workspace: String,
        name: String,
    },
    Status,
    Shutdown,
    Repositories,
    Register {
        source: String,
        name: Option<String>,
    },
    RenameRepository {
        repository: String,
        name: String,
    },
    Add {
        repository: String,
        name: String,
        base: Option<String>,
    },
    List,
    DiffBase {
        workspace: String,
    },
    ReservePort {
        workspace: String,
        name: String,
        port: Option<u16>,
        env_var: Option<String>,
        reason: Option<String>,
        on_conflict: Option<crate::repo_config::ConflictPolicy>,
    },
    Ports {
        workspace: Option<String>,
    },
    ReleasePort {
        workspace: String,
        name: String,
    },
    PortOverview {
        workspace: String,
    },
    Inspect {
        workspace: String,
    },
    Remove {
        workspace: String,
        choice: crate::removal::Choice,
        caller_pid: u32,
    },
    CheckRemoval {
        workspace: String,
        caller_pid: u32,
    },
    Stop {
        workspace: String,
    },
    Execute {
        workspace: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub protocol: u32,
    pub id: u64,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Body {
    ResourceLease(crate::resources::ResourceLease),
    ResourceLeases(Vec<crate::resources::ResourceLease>),
    ResourceOverview(crate::resources::Overview),
    ResourceBusy { message: String },
    Status(Status),
    Repositories(Vec<Repository>),
    Repository(Repository),
    Workspace(Workspace),
    Workspaces(Vec<Workspace>),
    Inspection(Inspection),
    Execution(ExecutionPlan),
    RemovalCheck(crate::removal::RemovalCheck),
    RemovalResult(crate::removal::RemovalResult),
    DiffBase(crate::model::DiffBase),
    Port(crate::model::PortReservation),
    Ports(Vec<crate::model::PortReservation>),
    PortSuggestion(crate::model::PortSuggestion),
    PortOverview(crate::model::PortOverview),
    Simulators(Vec<crate::simulators::Simulator>),
    Simulator(crate::simulators::Simulator),
    SimBusy { message: String },
    SimCatalog(serde_json::Value),
    SimHistory(Vec<crate::sim_audit::AuditEntry>),
    Ok,
    Error { code: String, message: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub version: String,
    pub uptime_secs: u64,
    pub managed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub exit_code: i32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Stop,
    Finished,
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
