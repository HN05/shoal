use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::model::{ExecutionPlan, Inspection, Repository, Workspace};

pub const VERSION: u32 = 2;
pub const MAX_FRAME: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub id: u64,
    pub method: Method,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Status,
    Shutdown,
    Repositories,
    Register {
        source: String,
    },
    Add {
        repository: String,
        name: String,
        base: Option<String>,
    },
    List,
    Inspect {
        workspace: String,
    },
    Remove {
        workspace: String,
        confirmed: bool,
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
    Status(Status),
    Repositories(Vec<Repository>),
    Repository(Repository),
    Workspace(Workspace),
    Workspaces(Vec<Workspace>),
    Inspection(Inspection),
    Execution(ExecutionPlan),
    RemovalCheck(crate::removal::RemovalCheck),
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
