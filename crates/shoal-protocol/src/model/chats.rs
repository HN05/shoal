//! Chat records and where a chat runs.
use super::{Completion, Repository, Space, Workspace};
use serde::{Deserialize, Serialize};
use shoal_ids::repository_ids::RepositoryId;

/// The mode of a chat recorded without one.
pub fn chat_mode() -> crate::config::modes::ModeName {
    crate::config::modes::ModeName::CHAT
}

/// An agent the user talks with in a repository's registered checkout, or
/// in a workspace's worktree without being its agent, until it is removed.
pub type Chat = Space<ChatPart>;

/// What only a chat has: its agent, its temporary directory and its
/// completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename = "chat")]
pub struct ChatPart {
    /// Its agent: the one its execution runs, else the one it saved last,
    /// else the one its queued launch starts; none once its saved sessions
    /// were discarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::agent::Agent>,
    pub started_at: crate::time::UnixTime,
    /// Its own directory for temporary files, which goes with it; none in a
    /// repository without a directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmp_dir: Option<std::path::PathBuf>,
    /// What `shoal done` recorded, until `shoal done --undo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<Completion>,
}

impl Chat {
    /// The mode its agent started in, which its restores keep.
    pub fn agent_mode(&self) -> crate::config::modes::ModeName {
        self.mode.clone().unwrap_or_else(chat_mode)
    }
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
            Self::Workspace(workspace) => Some(workspace.repository()),
            Self::Root(_) => None,
        }
    }

    /// The name a new chat here in `mode` takes when neither its start nor
    /// its issue names it: after the workspace whose worktree it runs in,
    /// the repository whose checkout it runs in and its mode, or its mode.
    pub fn chat_name(self, mode: &crate::config::modes::ModeName) -> String {
        match self {
            Self::Checkout(repository) => {
                format!("{} {mode}", repository.display_name())
            }
            Self::Workspace(workspace) => format!("{} chat", workspace.name),
            Self::Root(_) => mode.to_string(),
        }
    }

    pub fn workspace(self) -> Option<&'a Workspace> {
        match self {
            Self::Checkout(_) | Self::Root(_) => None,
            Self::Workspace(workspace) => Some(workspace),
        }
    }
}
