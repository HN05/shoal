use serde::{Deserialize, Serialize};

use super::Lifetime;

/// Requests about chats in registered checkouts.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMethod {
    /// Record a chat whose agent replaces process `pid`.
    StartChat {
        repository: crate::repository::Selector,
        agent: String,
        pid: u32,
    },
    /// Record that `chat`'s agent now runs as process `pid`, for an agent
    /// started detached instead of in place of the recording process.
    MoveChat {
        chat: String,
        pid: u32,
    },
    ListChats,
    /// The chat's undelivered messages about the workspaces it coordinates,
    /// without waiting; acknowledge them with
    /// [`Self::AcknowledgeChatMessages`]. It replaces a running wait.
    ChatUpdates {
        chat: String,
    },
    /// Long-lived: the chat's next messages, once it has any.
    WaitChatMessages {
        chat: String,
    },
    AcknowledgeChatMessages {
        chat: String,
        ids: Vec<i64>,
    },
}

impl ChatMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::WaitChatMessages { .. } => Lifetime::Resumed,
            _ => Lifetime::Reply,
        }
    }
}
