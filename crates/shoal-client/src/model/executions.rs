//! Executions and the plans and places they run with.
use super::{MergePlan, PortReservation, UnmergePlan, Workspace};
use crate::processes::Identity;
use crate::space_ids::SpaceId;
use crate::state::ExecutionState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Execution {
    pub id: String,
    pub workspace_id: SpaceId,
    pub state: ExecutionState,
    pub wrapper: Option<Identity>,
    pub child: Option<Identity>,
    pub group_id: Option<u32>,
    /// Where it runs in a terminal host, when its wrapper runs in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::sessions::Origin>,
    /// The agent it runs, when it runs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The model and effort its agent runs with.
    #[serde(default, flatten)]
    pub model: crate::agent::ModelChoice,
    /// The mode its agent started in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The `--permission` narrowing its agent started with, which narrows the
    /// permissions of its mode.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// Why Shoal stopped its agent, while its wrapper waits to restore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<crate::protocol::Protection>,
}

/// Everything the execution wrapper needs to launch a tracked command.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub id: String,
    pub workspace: Workspace,
    pub scope_token: String,
    /// Absolute setup command path when the execution runs configured setup.
    pub setup_cmd: Option<PathBuf>,
    /// Copy the main checkout's ignored files before the setup command.
    pub copy_ignored: bool,
    pub ports: Vec<PortReservation>,
    #[serde(default)]
    pub environment: std::collections::BTreeMap<String, std::ffi::OsString>,
    pub merge: Option<Box<MergePlan>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmerge: Option<Box<UnmergePlan>>,
}

/// A place Shoal created for a workspace, which closes with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedPlace {
    pub workspace_id: SpaceId,
    pub origin: crate::sessions::Origin,
}
