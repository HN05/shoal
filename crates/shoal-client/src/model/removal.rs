//! Removed spaces and what removal leaves: kept branches and worktrees
//! no workspace owns.
use super::{Checkpoint, OpenedWorkspace, SpaceLinks, SpaceRef, StartingCommit};
use crate::repository_ids::RepositoryId;
use crate::space_ids::SpaceId;
use crate::space_names::SpaceName;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use shoal_refs::{BranchName, CommitId};

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
    pub branch: BranchName,
    /// The commit removal left the branch on.
    pub head: CommitId,
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
    pub branch: BranchName,
    pub head: CommitId,
}

/// What `shoal restore` recreates of a removed space until `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedSpace {
    /// Named as removal records and exports of workspaces have always
    /// named it.
    #[serde(rename = "workspace_id")]
    pub space_id: SpaceId,
    /// None for a chat without a repository.
    pub repository_id: Option<RepositoryId>,
    pub name: SpaceName,
    /// The space it was below, which a restore puts it below again while
    /// that takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SpaceRef>,
    pub links: SpaceLinks,
    /// Agents stopped by the removal or before it, which `shoal restore`
    /// continues once the space is restored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<crate::executions::StoppedAgent>,
    /// The last sessions of agents, which `shoal restore` continues once the
    /// space is restored.
    #[serde(
        default,
        alias = "last_session",
        deserialize_with = "crate::executions::one_or_many_sessions",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub last_sessions: Vec<crate::executions::LastSession>,
    pub cause: crate::events::EventCause,
    pub removed_at: crate::time::UnixTime,
    pub expires_at: crate::time::UnixTime,
    /// What only its kind keeps.
    #[serde(flatten)]
    pub kind: RemovedKind,
}

/// What a removal keeps of a space of one kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RemovedKind {
    Chat {
        chat: Box<KeptChat>,
    },
    /// Its fields sit beside the shared ones, as in the records workspaces
    /// had before chats had any.
    Workspace(Box<KeptWorkspace>),
}

/// What a removal keeps of a chat only: how its agent ran, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptChat {
    pub agent: crate::agent::Agent,
    /// The mode its agent ran in, which its restore keeps.
    pub mode: crate::config::modes::ModeName,
    /// The permissions `--permission` gave its agent.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// The workspace whose worktree it ran in; none for a chat in the
    /// registered checkout or without a repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<super::WorkspaceRef>,
    /// The `root_dir` a chat without a repository ran in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_dir: Option<std::path::PathBuf>,
    /// Where it ran in a terminal host, when it ran in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::sessions::Origin>,
    /// The agent session its hooks reported last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<crate::agent::AgentSessionId>,
}

/// What a removal keeps of a workspace only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptWorkspace {
    /// The checkpoint taken at removal.
    pub checkpoint: Checkpoint,
    pub base_ref: Option<String>,
    pub base_commit: Option<CommitId>,
    /// Where it started, for a workspace at a commit, which restore opens
    /// without a branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<StartingCommit>,
    /// Whether it was created without setup, which its restore skips too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub setup_skipped: bool,
    /// The agents' session files an import carried, which the daemon keeps
    /// until the restore that opens the workspace puts them in place with
    /// its settings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_files: Vec<crate::transfer::SessionFile>,
}

impl RemovedSpace {
    /// What it keeps as a workspace; `None` for a chat.
    pub fn workspace(&self) -> Option<&KeptWorkspace> {
        match &self.kind {
            RemovedKind::Workspace(workspace) => Some(workspace),
            RemovedKind::Chat { .. } => None,
        }
    }

    /// What it keeps as a chat; `None` for a workspace.
    pub fn chat(&self) -> Option<&KeptChat> {
        match &self.kind {
            RemovedKind::Chat { chat } => Some(chat),
            RemovedKind::Workspace(_) => None,
        }
    }

    /// What it keeps as a workspace, which a chat refuses.
    pub fn kept_workspace(&self) -> anyhow::Result<&KeptWorkspace> {
        (self.workspace()).with_context(|| format!("{} was not a workspace", self.space_id))
    }

    /// [`Self::kept_workspace`], to change.
    pub fn kept_workspace_mut(&mut self) -> anyhow::Result<&mut KeptWorkspace> {
        match &mut self.kind {
            RemovedKind::Workspace(workspace) => Ok(workspace),
            RemovedKind::Chat { .. } => anyhow::bail!("{} was not a workspace", self.space_id),
        }
    }

    /// Its repository, which a chat without one refuses.
    pub fn repository(&self) -> anyhow::Result<&RepositoryId> {
        (self.repository_id.as_ref())
            .with_context(|| format!("{} belonged to no repository", self.space_id))
    }

    /// Whether it is a copy of another space, whose agents start from forks
    /// of the original's sessions.
    pub fn is_copy(&self) -> bool {
        self.agents
            .iter()
            .any(|agent| agent.record.copied.is_some())
            || self.last_sessions.iter().any(|last| last.copied.is_some())
    }
}

/// A workspace `shoal restore` opened again from its removal record; the
/// caller sets it up and links its PRs.
#[derive(Debug, Serialize, Deserialize)]
pub struct RestoredWorkspace {
    pub opened: OpenedWorkspace,
    pub removed: RemovedSpace,
    /// The session files the daemon kept with the removal record, which
    /// the caller puts in place and then deletes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_files: Vec<crate::transfer::CarriedSessionFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
