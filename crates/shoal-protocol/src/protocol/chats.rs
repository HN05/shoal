use serde::{Deserialize, Serialize};

/// Requests about chats.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMethod {
    /// Refuse a `parent` a new chat in `repository`, or without one, in the
    /// worktree of `workspace` when given, could not go below.
    CheckChatParent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<crate::space_names::SpaceSelector>,
        /// The space the chat goes below.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<crate::space_names::SpaceSelector>,
    },
    /// `root_dir`, where a chat without a repository runs, prepared as
    /// registering a repository prepares it.
    ChatRoot,
}

/// The answer to [`ChatMethod::ChatRoot`].
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatRoot {
    pub path: std::path::PathBuf,
}
