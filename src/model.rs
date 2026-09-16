use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: String,
    pub path: PathBuf,
    pub source: String,
    pub last_used: i64,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub repository_id: String,
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub state: crate::state::WorkspaceState,
    pub error: Option<String>,
    pub base_commit: Option<String>,
    pub base_ref: Option<String>,
    pub git_dir: Option<PathBuf>,
    pub git_dir_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Execution {
    pub id: String,
    pub workspace_id: String,
    pub state: crate::state::ExecutionState,
    pub wrapper: Option<crate::process_identity::Identity>,
    pub child: Option<crate::process_identity::Identity>,
    pub group_id: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Inspection {
    pub resources: Vec<crate::resources::ResourceLease>,
    pub simulators: Vec<crate::simulators::Simulator>,
    pub workspace: Workspace,
    pub executions: Vec<Execution>,
    pub ports: Vec<PortReservation>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub scope_token: String,
    pub id: String,
    pub workspace: Workspace,
    pub ports: Vec<PortReservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortReservation {
    pub workspace_id: String,
    pub name: String,
    pub port: u16,
    pub env_var: String,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiffBase {
    pub workspace_id: String,
    pub commit: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortSuggestion {
    pub workspace_id: String,
    pub name: String,
    pub requested_port: u16,
    pub suggested_port: u16,
    pub env_var: String,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortOverview {
    pub workspace: Workspace,
    pub reserved: Vec<PortReservation>,
    pub configured: std::collections::BTreeMap<String, crate::repo_config::PortDefinition>,
    pub on_conflict: crate::repo_config::ConflictPolicy,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PulledMain {
    pub repository_id: String,
    pub previous_commit: String,
    pub commit: String,
    pub updated: bool,
}
