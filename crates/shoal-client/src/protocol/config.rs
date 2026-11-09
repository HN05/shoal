//! Whose repository config a request reads.
use serde::{Deserialize, Serialize};

/// Whose repository config: a workspace's worktree file under the saved
/// config, the registered checkout's before a workspace exists, or none,
/// for the global config alone.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigTarget {
    Workspace(crate::space_names::SpaceSelector),
    Repository(crate::repository::Selector),
    Global,
}

impl ConfigTarget {
    /// The config of `chat`: its repository's, or the global config alone
    /// for a chat without a repository.
    pub fn chat(chat: &crate::model::Chat) -> Self {
        match &chat.repository_id {
            Some(id) => Self::Repository(id.clone().into()),
            None => Self::Global,
        }
    }
}
