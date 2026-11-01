//! Manual cleanup: what a request covers and what it removed, kept or emptied.
use serde::{Deserialize, Serialize};

use crate::model::CleanupReason;

/// The workspaces a manual cleanup removed, or would remove in a dry run,
/// and those it retained because inspection or removal failed. A dry run
/// also lists why it keeps each other workspace.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ManualCleanup {
    pub dry_run: bool,
    pub removed: Vec<String>,
    pub retained: Vec<Retained>,
    #[serde(default)]
    pub kept: Vec<Kept>,
    #[serde(default)]
    pub caches: Vec<CacheCleanup>,
}

/// Which items of one kind a cleanup request covers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    #[default]
    None,
    All,
    /// Workspaces, or the repositories whose caches to empty, by name or ID.
    Only(Vec<String>),
}

/// What a cleanup request covers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub workspaces: Pick,
    #[serde(default)]
    pub caches: Pick,
    /// Only this repository's workspaces and caches, by any repository selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Retained {
    pub workspace: String,
    pub error: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Kept {
    pub workspace: String,
    pub reason: CleanupReason,
}

/// One repository's shared caches on a cleanup request.
#[derive(Debug, Serialize, Deserialize)]
pub struct CacheCleanup {
    pub repository: String,
    pub repository_id: String,
    #[serde(flatten)]
    pub emptying: Emptying,
}

/// What happened to one repository's shared caches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Emptying {
    /// The caches emptied, or that a dry run would empty.
    Emptied {
        caches: Vec<String>,
    },
    /// A tracked execution or the seed build of the repository is running.
    Busy {
        caches: Vec<String>,
    },
    Failed {
        error: String,
    },
}
