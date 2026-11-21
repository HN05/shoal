use super::{Forwarding, Target};
use serde::{Deserialize, Serialize};

/// Requests about chats.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMethod {
    /// `root_dir`, where a chat without a repository runs, prepared as
    /// registering a repository prepares it.
    ChatRoot,
}

/// The answer to [`ChatMethod::ChatRoot`].
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatRoot {
    pub path: std::path::PathBuf,
}

impl ChatMethod {
    /// `root_dir` is the caller's machine's.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ChatRoot => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ChatRoot => None,
        }
    }
}
