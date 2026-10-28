use serde::{Deserialize, Serialize};

use super::Lifetime;

/// Requests about chats in registered checkouts.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMethod {
    /// Record a chat named `name` whose agent replaces process `pid`.
    StartChat {
        repository: crate::repository::Selector,
        name: String,
        agent: String,
        pid: u32,
        /// Where the chat runs in a terminal host.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<crate::sessions::Origin>,
    },
    /// Record that `chat`'s agent now runs as process `pid`, for an agent
    /// started detached instead of in place of the recording process.
    MoveChat {
        chat: String,
        pid: u32,
    },
    ListChats,
    /// Refuse a name a new chat could not take now.
    CheckChatName {
        name: String,
    },
    /// `name`, or its lowest `-2`, `-3`, ... variant, that a new chat could
    /// take now.
    FreeChatName {
        name: String,
    },
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
