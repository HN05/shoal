//! Registered repositories and changes to their registration.
use super::Execution;
use crate::forge::repo::remote_url::source_name;
use serde::{Deserialize, Serialize};
use shoal_ids::repository_ids::RepositoryId;
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
    /// URL clone. Reserved at registration and moved only into a new
    /// `root_dir`.
    pub workspaces_dir: PathBuf,
    /// Registered by `shoal contribute`: removed once nothing of it is left.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub contribution: bool,
    /// The URL the other machines of the hub clone it from, while it is
    /// shared with them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_url: Option<String>,
}

impl Repository {
    /// The explicit name, or the last path component of the source.
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .unwrap_or_else(|| source_name(&self.source))
    }
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

/// What removing a repository would remove on its machine: its spaces,
/// each after those below it, with the work of their own it would lose.
#[derive(Debug, Serialize, Deserialize)]
pub struct RepositoryRemovalPreview {
    pub repository: Repository,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spaces: Vec<SpaceWork>,
}

/// A space a repository's removal removes.
#[derive(Debug, Serialize, Deserialize)]
pub struct SpaceWork {
    #[serde(flatten)]
    pub space: super::SpaceRef,
    pub number: shoal_ids::space_ids::SpaceNumber,
    /// What its worktree holds that neither a branch nor a remote keeps;
    /// none for a chat, or a worktree that could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work: Option<WorktreeWork>,
}

/// What a worktree holds that neither a branch nor a remote keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeWork {
    /// It has uncommitted or untracked files.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dirty: bool,
    /// Commits no remote or default branch has.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unpushed_commits: u64,
}

fn is_zero(count: &u64) -> bool {
    *count == 0
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
    /// The chats without a repository whose files move with the root,
    /// running, stopped or removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<crate::space_names::SpaceName>,
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
