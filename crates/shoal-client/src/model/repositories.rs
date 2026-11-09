//! Registered repositories and changes to their registration.
use super::Execution;
use crate::repository_ids::RepositoryId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: RepositoryId,
    pub path: PathBuf,
    pub source: String,
    /// Monotonic usage counter; higher means more recently used.
    pub last_used: i64,
    pub name: Option<String>,
    /// `<root_dir>/<name>`: parent of this repository's workspaces and of its
    /// URL clone. Reserved once and moved only into a new `root_dir`; `None`
    /// until first needed.
    pub workspaces_dir: Option<PathBuf>,
    /// Registered by `shoal contribute`: removed once nothing of it is left.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub contribution: bool,
}

/// A saved edit of a repository's local config and the backup it kept.
#[derive(Debug, Serialize, Deserialize)]
pub struct LocalConfigEdit {
    pub config: PathBuf,
    pub backup: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepositoryRemoval {
    pub removed: bool,
    pub repository_id: RepositoryId,
    pub path: PathBuf,
    pub workspaces_removed: usize,
    /// Canonical worktree paths removed during this operation and prior retry attempts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_paths: Vec<PathBuf>,
}

/// The repository directories a move would take into `directory`, the
/// canonical directory that holds `root_dir`'s repositories.
#[derive(Debug, Serialize, Deserialize)]
pub struct RepositoryMoves {
    pub directory: PathBuf,
    pub repositories: Vec<Repository>,
    /// The commands and agents running in their workspaces, which must stop
    /// before the move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executions: Vec<Execution>,
}

/// A repository directory moved into the configured `root_dir`.
#[derive(Debug, Serialize, Deserialize)]
pub struct MovedRepository {
    /// The registration as recorded after the move.
    pub repository: Repository,
    pub from: PathBuf,
    /// The workspace paths before the move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_paths: Vec<PathBuf>,
}
