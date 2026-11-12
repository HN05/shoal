//! Executions and the plans and places they run with.
use super::{MergePlan, PortReservation, UnmergePlan, Workspace};
use crate::execution_ids::ExecutionId;
use crate::processes::Identity;
use crate::space_ids::{SpaceId, SpaceNumber};
use crate::state::ExecutionState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Execution {
    pub id: ExecutionId,
    /// The space it runs in, a workspace or a chat.
    pub space_id: SpaceId,
    pub state: ExecutionState,
    pub wrapper: Option<Identity>,
    pub child: Option<Identity>,
    pub group_id: Option<u32>,
    /// Where it runs in a terminal host, when its wrapper runs in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::sessions::Origin>,
    /// The agent it runs, when it runs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::agent::Agent>,
    /// The model and effort its agent runs with.
    #[serde(default, flatten)]
    pub model: crate::agent::ModelChoice,
    /// The mode its agent started in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::config::modes::ModeName>,
    /// The permissions `--permission` gave its agent, which apply as
    /// given whatever its mode's.
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
/// What only a workspace has, such as setup, ports and merges, is absent
/// for a chat.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub id: ExecutionId,
    pub space: ExecutionSpace,
    pub scope_token: crate::scope_tokens::ScopeToken,
    /// Absolute setup command path when the execution runs configured setup.
    pub setup_cmd: Option<PathBuf>,
    /// Copy the main checkout's ignored files before the setup command.
    pub copy_ignored: Option<CopyIgnored>,
    pub ports: Vec<PortReservation>,
    #[serde(default)]
    pub environment: std::collections::BTreeMap<String, std::ffi::OsString>,
    pub merge: Option<Box<MergePlan>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmerge: Option<Box<UnmergePlan>>,
}

/// The space an execution runs in, a workspace or a chat, as its wrapper
/// reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSpace {
    pub id: SpaceId,
    pub number: SpaceNumber,
    /// The directory its commands run in.
    pub path: PathBuf,
}

impl ExecutionSpace {
    /// The name of the directory its commands run in.
    pub fn directory_name(&self) -> &str {
        super::workspaces::directory_name(&self.path)
    }
}

impl From<&Workspace> for ExecutionSpace {
    fn from(workspace: &Workspace) -> Self {
        Self {
            id: workspace.id.clone(),
            number: workspace.number,
            path: workspace.path.clone(),
        }
    }
}

/// Setup's copy of the main checkout's ignored files.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyIgnored {
    /// Gitignore-style patterns of the entries it leaves out.
    pub exclude: Vec<String>,
}

/// A place Shoal created for a space, a workspace or a chat, which closes
/// with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedPlace {
    pub space_id: SpaceId,
    pub origin: crate::sessions::Origin,
}
