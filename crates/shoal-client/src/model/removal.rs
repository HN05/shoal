//! Removed workspaces and what removal leaves: kept branches and worktrees
//! no workspace owns.
use super::{Checkpoint, OpenedWorkspace, SpaceLinks, StartingCommit};
use crate::repository_ids::RepositoryId;
use crate::space_ids::SpaceId;
use crate::space_names::SpaceName;
use serde::{Deserialize, Serialize};

/// A Git worktree under a repository's Shoal directory that no workspace owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntrackedWorktree {
    pub repository_id: RepositoryId,
    pub path: std::path::PathBuf,
}

/// A branch removal kept because only it has some commits: none of them is on
/// a remote or the default branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptBranch {
    pub repository_id: RepositoryId,
    /// The repository's name, or its path when it has none.
    pub repository: String,
    pub branch: String,
    /// The commit removal left the branch on.
    pub head: String,
    pub kept_at: crate::time::UnixTime,
    /// Commits only this branch has.
    pub unpushed_commits: u64,
    pub committed_at: crate::time::UnixTime,
    pub subject: String,
}

/// A kept branch to delete, at the commit the user saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchTarget {
    pub repository_id: RepositoryId,
    pub branch: String,
    pub head: String,
}

/// What `shoal work restore` recreates of a removed workspace until `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedWorkspace {
    pub workspace_id: SpaceId,
    pub repository_id: RepositoryId,
    pub name: SpaceName,
    /// The checkpoint taken at removal.
    pub checkpoint: Checkpoint,
    pub base_ref: Option<String>,
    pub base_commit: Option<String>,
    /// Where it started, for a workspace at a commit, which restore opens
    /// without a branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<StartingCommit>,
    pub links: SpaceLinks,
    /// Agents stopped by the removal or before it, which `shoal resume`
    /// continues once the workspace is restored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<crate::executions::StoppedAgent>,
    /// The last sessions of agents, which `shoal resume` continues once the
    /// workspace is restored.
    #[serde(
        default,
        alias = "last_session",
        deserialize_with = "crate::executions::one_or_many_sessions",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub last_sessions: Vec<crate::executions::LastSession>,
    /// The agents' session files an import carried, which the daemon keeps
    /// until the restore that opens the workspace puts them in place with
    /// its settings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_files: Vec<crate::transfer::SessionFile>,
    pub cause: crate::events::EventCause,
    pub removed_at: crate::time::UnixTime,
    pub expires_at: crate::time::UnixTime,
}

impl RemovedWorkspace {
    /// Whether it is a copy of another workspace, whose agents start from
    /// forks of the original's sessions.
    pub fn is_copy(&self) -> bool {
        self.agents
            .iter()
            .any(|agent| agent.record.copied.is_some())
            || self.last_sessions.iter().any(|last| last.copied.is_some())
    }
}

/// A workspace `shoal work restore` opened again from its removal record; the
/// caller sets it up and links its PRs.
#[derive(Debug, Serialize, Deserialize)]
pub struct RestoredWorkspace {
    pub opened: OpenedWorkspace,
    pub removed: RemovedWorkspace,
    /// The session files the daemon kept with the removal record, which
    /// the caller puts in place and then deletes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_files: Vec<crate::transfer::CarriedSessionFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
