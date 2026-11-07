//! Explicit recovery: what to repair, and what an inspection found.
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::client::space_names::SpaceSelector;
use crate::{
    client::{
        connection::{Daemon, request},
        processes::Identity,
    },
    model::{Checkpoint, RestoredCheckpoint, Workspace},
    protocol::RecoveryMethod,
    state::{ExecutionState, states},
};

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
    pub id: String,
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

/// Keep `workspace`'s HEAD and unsaved files as a checkpoint named `name`.
pub async fn checkpoint(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: Option<String>,
) -> Result<Checkpoint> {
    request(daemon, RecoveryMethod::Checkpoint { workspace, name }).await
}

/// Every checkpoint of `workspace`, read page by page; `None` when the
/// workspace has none to restore.
pub async fn checkpoints(daemon: &impl Daemon, workspace: &str) -> Result<Option<Vec<Checkpoint>>> {
    let mut checkpoints = Vec::new();
    loop {
        let after = checkpoints
            .last()
            .map(|checkpoint: &Checkpoint| checkpoint.number);
        let method = RecoveryMethod::Checkpoints {
            workspace: SpaceSelector::parse(workspace),
            after,
        };
        let Some(page) = request::<Option<Vec<Checkpoint>>>(daemon, method).await? else {
            return Ok(None);
        };
        if page.is_empty() {
            return Ok(Some(checkpoints));
        }
        checkpoints.extend(page);
    }
}

/// Return `workspace` to its checkpoint `checkpoint`, keeping the current
/// state in a new one first.
pub async fn restore_checkpoint(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    checkpoint: u64,
) -> Result<RestoredCheckpoint> {
    let method = RecoveryMethod::RestoreCheckpoint {
        workspace,
        checkpoint,
    };
    request(daemon, method).await
}

/// Inspect `workspace`, or every workspace, and repair what `options` asks.
pub async fn reconcile(
    daemon: &impl Daemon,
    workspace: Option<SpaceSelector>,
    options: ReconcileOptions,
) -> Result<Vec<Report>> {
    request(daemon, RecoveryMethod::Reconcile { workspace, options }).await
}
