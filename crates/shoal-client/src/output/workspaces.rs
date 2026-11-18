//! What workspace commands print.
use shoal_protocol::space_names::SpaceName;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::output::{batch::Failure, outputs};
use shoal_protocol::{
    hub::directory::Listed,
    model::{Completion, Space, SpaceHold, WorkspaceRef},
    removal::RemovalResult,
};

/// A space `shoal list` prints: this machine's, or another joined
/// machine's as that machine last reported it, which names it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant, reason = "a listing holds a few spaces")]
pub enum ListedSpace {
    Elsewhere(Listed),
    Here(Space),
}

/// A removed space, or one being removed, and the holds it had.
#[derive(Debug, Serialize, Deserialize)]
pub struct Removed {
    /// The space's name, which a removal of several reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<SpaceName>,
    /// What the removal did, once it finished.
    #[serde(flatten)]
    pub removal: Option<RemovalResult>,
    /// The removal goes on in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<BackgroundRemoval>,
    pub holds: Vec<SpaceHold>,
    /// Git no longer knew its worktree, so the removal deleted what was
    /// left of it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disowned: bool,
}

impl Removed {
    /// The spaces below it the removal took, or takes, first.
    pub fn children(&self) -> &[SpaceName] {
        match (&self.removal, &self.background) {
            (Some(removal), _) => &removal.children,
            (None, Some(background)) => &background.children,
            (None, None) => &[],
        }
    }
}

/// A removal that goes on after the command returned.
#[derive(Debug, Serialize, Deserialize)]
pub struct BackgroundRemoval {
    /// The spaces below it it takes first, each after those below it.
    pub children: Vec<SpaceName>,
}

/// The spaces a removal of several removed.
#[derive(Debug, Serialize, Deserialize)]
pub struct Removals {
    pub removed: Vec<Removed>,
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

/// Whether an item is linked, and how, with what assigning or unassigning
/// an issue on the forge warned of.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registered {
    pub registered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<shoal_protocol::model::Relation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A question asked in the agent's chat.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct InChat {
    pub in_chat: bool,
}

/// A start handed to a place beside the caller's terminal, which opens the
/// workspace and starts its agent there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartedBeside {
    /// The name the workspace is planned under.
    pub workspace: SpaceName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<shoal_protocol::agent::Agent>,
}

outputs!(
    ListedSpace,
    Removed,
    Removals,
    Directory,
    Withdrawn,
    Revoked,
    Bases,
    Registered,
    InChat,
    StartedBeside
);
