use serde::{Deserialize, Serialize};

use super::Lifetime;

/// Requests about chats in registered checkouts.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMethod {
    /// Record a chat named `name` whose agent replaces process `pid`.
    StartChat {
        repository: crate::client::repository::Selector,
        name: String,
        agent: String,
        pid: u32,
        /// Where the chat runs in a terminal host.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<crate::client::sessions::Origin>,
        /// The recording process hands the chat to a session it starts,
        /// with [`Self::MoveChat`]; until then the chat keeps its name.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        hands_over: bool,
    },
    /// Run the saved chat `chat`, by ID, again as process `pid`, as
    /// [`Self::StartChat`] records a new one.
    RestoreChat {
        chat: String,
        pid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<crate::client::sessions::Origin>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        hands_over: bool,
    },
    /// Process `pid` could not restore the saved chat `chat`, failing with
    /// `error`; the chat waits for `shoal resume` again if `pid` had taken
    /// it over.
    SaveChat {
        chat: String,
        pid: u32,
        error: String,
    },
    /// Record that `chat`'s agent now runs as process `pid`, for an agent
    /// started detached instead of in place of the recording process.
    MoveChat {
        chat: String,
        pid: u32,
    },
    ListChats,
    /// End the saved chat `chat`, by ID or name, without restoring it.
    DiscardChat {
        chat: String,
    },
    /// Give the running chat `chat`, by ID or name, the name `name`.
    RenameChat {
        chat: String,
        name: String,
    },
    /// A message from the user to the agent of the running chat `chat`, by
    /// ID or name, which other senders send as [`super::AgentMethod::SendAgentMessage`].
    SendChatMessage {
        chat: String,
        message: String,
    },
    /// Refuse a name a new chat could not take now.
    CheckChatName {
        name: String,
    },
    /// `name`, or its lowest `-2`, `-3`, ... variant, that a new chat could
    /// take now.
    FreeChatName {
        name: String,
    },
    /// The chat's undelivered messages, oldest first, leaving a running wait
    /// for them in place.
    ChatMessages {
        chat: String,
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
        /// The PIDs of the leftovers the hook printed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        leftovers: Vec<u32>,
        /// The deliveries of the item updates the hook printed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deliveries: Vec<String>,
    },
    /// Record the running chat `chat`, by ID or name, as done, handing back
    /// `results`; the reply is the [`Completion`](crate::model::Completion).
    ChatDone {
        chat: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        results: Vec<crate::client::files::NewFile>,
        /// The chat whose agent sends the request, which attaches them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_chat: Option<String>,
    },
    /// Withdraw the completion the running chat `chat` recorded.
    ChatUndone {
        chat: String,
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
