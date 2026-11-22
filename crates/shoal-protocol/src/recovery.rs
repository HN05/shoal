//! Explicit recovery: what to repair, and what an inspection found.
use shoal_ids::execution_ids::ExecutionId;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use shoal_states::states;

use crate::{model::Workspace, processes::Identity, state::ExecutionState};

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct ReconcileOptions {
    /// Apply safe state repairs instead of only reporting.
    pub repair: bool,
    /// Stop connected commands and identity-verified survivors first.
    pub stop: bool,
    /// The caller checked that untracked processes have stopped.
    pub acknowledge_stopped: bool,
    /// The caller checked that the worktree at the recorded path is this
    /// workspace's; re-establish ownership when verification fails.
    #[serde(default)]
    pub reclaim: bool,
}

states!(DirectoryState {
    Valid => "valid",
    Missing => "missing",
    Moved => "moved",
    Unverified => "unverified",
});

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecutionReport {
    pub id: ExecutionId,
    pub state: ExecutionState,
    pub connected: bool,
    pub wrapper_alive: bool,
    pub processes: Vec<Identity>,
    pub unverified_processes: Vec<Identity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable_processes: Vec<Identity>,
    pub cleared: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub workspace: Workspace,
    pub directory: DirectoryState,
    pub moved_to: Option<PathBuf>,
    pub executions: Vec<ExecutionReport>,
    pub changes: Vec<String>,
    pub issues: Vec<String>,
}

impl Report {
    /// The problems `reports` leave, joined for a message.
    pub fn issues_of(reports: &[Self]) -> String {
        reports
            .iter()
            .flat_map(|report| report.issues.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("; ")
    }
}
