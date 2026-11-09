//! What workspace commands print.
use crate::space_names::SpaceName;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    model::{Completion, WorkspaceHold, WorkspaceRef},
    output::{batch::Failure, outputs},
    removal::RemovalResult,
};

/// A removed workspace, and the holds it had.
#[derive(Debug, Serialize, Deserialize)]
pub struct Removed {
    /// The workspace's name, which a removal of several reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<SpaceName>,
    #[serde(flatten)]
    pub removal: RemovalResult,
    pub holds: Vec<WorkspaceHold>,
}

/// The spaces a removal of several removed.
#[derive(Debug, Serialize, Deserialize)]
pub struct Removals {
    pub removed: Vec<Removed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<crate::output::chat::RemovedChat>,
    pub failed: Vec<Failure>,
}

/// A workspace's directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Directory {
    pub path: PathBuf,
}

/// A recorded completion, withdrawn.
#[derive(Debug, Serialize, Deserialize)]
pub struct Withdrawn {
    pub withdrawn: Completion,
}

/// Whether an exported environment's scope was revoked.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Revoked {
    pub revoked: bool,
}

/// The workspace a workspace builds on, and those built on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bases {
    pub base_workspace: Option<WorkspaceRef>,
    pub stacked_workspaces: Vec<WorkspaceRef>,
}

/// Whether an item is linked, and how.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Registered {
    pub registered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<crate::model::Relation>,
}

/// A question asked in the agent's chat.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct InChat {
    pub in_chat: bool,
}

outputs!(
    Removed, Removals, Directory, Withdrawn, Revoked, Bases, Registered, InChat
);
