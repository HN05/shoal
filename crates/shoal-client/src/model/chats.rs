//! Chat records and where a chat runs.
use super::{
    AgentStatus, Completion, Repository, ReviewMark, SpaceLinks, SpaceRef, Workspace, WorkspaceRef,
};
use crate::agent::AgentSessionId;
use crate::repository_ids::RepositoryId;
use crate::space_ids::{SpaceId, SpaceNumber};
use crate::space_names::SpaceName;
use serde::{Deserialize, Serialize};

/// The mode of a chat recorded without one.
pub fn chat_mode() -> crate::config::modes::ModeName {
    crate::config::modes::ModeName::CHAT
}

/// An agent the user talks with in a repository's registered checkout, or
/// in a workspace's worktree without being its agent, until it is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chat {
    pub id: SpaceId,
    /// Unique among the spaces that exist, workspaces and chats; freed when
    /// it goes.
    #[serde(default)]
    pub number: SpaceNumber,
    /// The repository it belongs to; none for a chat that manages the
    /// spaces of every repository.
    pub repository_id: Option<RepositoryId>,
    /// Unique among workspace names and the names of other chats.
    pub name: SpaceName,
    pub agent: crate::agent::Agent,
    /// The mode its agent started in, which its restores keep.
    #[serde(default = "chat_mode")]
    pub mode: crate::config::modes::ModeName,
    /// The permissions `--permission` gave its agent, which apply as
    /// given whatever its mode's.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// The workspace whose worktree it runs in; none for a chat in the
    /// registered checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceRef>,
    pub pid: u32,
    pub started_at: crate::time::UnixTime,
    /// Where it runs in a terminal host, when it runs in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::sessions::Origin>,
    /// Its own directory for temporary files, which goes with it; none in a
    /// repository without a directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmp_dir: Option<std::path::PathBuf>,
    /// The `root_dir` a chat without a repository started in, where it runs
    /// and keeps its temporary directory however `root_dir` changes later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_dir: Option<std::path::PathBuf>,
    /// The agent session its hooks reported last, which an export continues.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AgentSessionId>,
    /// How the chat comes back once its agent stopped, by exiting or as
    /// Shoal saved its session; none while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<crate::state::ChatRestore>,
    /// The issues, PRs and CI runs it links.
    #[serde(default)]
    pub links: SpaceLinks,
    /// Its ready-for-review marks, on its linked items or on itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review: Vec<ReviewMark>,
    /// What `shoal done` recorded, until `shoal done --undo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<Completion>,
    /// The turn state its running agent last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<AgentStatus>,
    /// Where its agent is in its work, as it last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<crate::progress::Progress>,
    /// The space above it.
    #[serde(default)]
    pub parent: Option<SpaceRef>,
    /// The spaces below it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SpaceRef>,
}

/// Where a new chat runs: its repository's registered checkout, the
/// worktree of a workspace in it, or for a chat without a repository,
/// `root_dir`.
#[derive(Debug, Clone, Copy)]
pub enum ChatHome<'a> {
    Checkout(&'a Repository),
    Workspace(&'a Workspace),
    Root(&'a std::path::Path),
}

impl<'a> ChatHome<'a> {
    /// The directory the chat's agent runs in.
    pub fn path(self) -> &'a std::path::Path {
        match self {
            Self::Checkout(repository) => &repository.path,
            Self::Workspace(workspace) => &workspace.path,
            Self::Root(root) => root,
        }
    }

    /// The chat's repository; none at `root_dir`.
    pub fn repository_id(self) -> Option<&'a RepositoryId> {
        match self {
            Self::Checkout(repository) => Some(&repository.id),
            Self::Workspace(workspace) => Some(&workspace.repository_id),
            Self::Root(_) => None,
        }
    }

    pub fn workspace(self) -> Option<&'a Workspace> {
        match self {
            Self::Checkout(_) | Self::Root(_) => None,
            Self::Workspace(workspace) => Some(workspace),
        }
    }
}

/// A chat after a rename, with the name it had before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenamedChat {
    pub chat: Chat,
    pub previous_name: SpaceName,
}
