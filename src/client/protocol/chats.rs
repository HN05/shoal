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
    /// Refuse a name a new chat could not take now.
    CheckChatName {
        name: String,
    },
    /// `name`, or its lowest `-2`, `-3`, ... variant, that a new chat could
    /// take now.
    FreeChatName {
        name: String,
    },
}

impl ChatMethod {
    pub fn lifetime(&self) -> Lifetime {
        Lifetime::Reply
    }
}
