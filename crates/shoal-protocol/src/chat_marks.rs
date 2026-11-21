//! The mark an agent puts on its turn with `shoal send --in-chat`: once
//! the turn ends, its final message goes as a post in a thread or as an
//! alert. A turn has one mark, which a later one replaces.
use serde::{Deserialize, Serialize};

use crate::{alerts::Recipient, item_numbers::ItemNumber};

/// What a turn's final message becomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatMark {
    /// A post in this thread.
    Post { thread: ItemNumber },
    /// An alert to these recipients.
    Alert { to: MarkedRecipients },
}

/// Whom a turn's alert reaches, as the daemon found them when the turn was
/// marked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MarkedRecipients {
    /// The user, with no agent above to coordinate the space.
    User,
    /// The agent of this space above, which coordinates the sender's.
    Coordinator { space: Recipient },
    /// The coordinator on another machine.
    Above,
    /// The agent of this space below, on the machine of the hub so named
    /// when it is not this one.
    Space {
        space: Recipient,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<String>,
    },
    /// Every agent the sender's scope reaches but its own, of this
    /// repository when it names one.
    All {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<String>,
    },
}

impl MarkedRecipients {
    /// Whom they are, as text that follows "to".
    pub fn describe(&self) -> String {
        match self {
            Self::User => "the user".into(),
            Self::Coordinator { space }
            | Self::Space {
                space,
                machine: None,
            } => format!("the agent of {}", space.label()),
            Self::Space {
                space,
                machine: Some(machine),
            } => format!("the agent of {} on {machine}", space.label()),
            Self::Above => "the coordinating agent on another machine".into(),
            Self::All { repository: None } => "every agent your scope reaches".into(),
            Self::All {
                repository: Some(repository),
            } => format!("every agent of {repository} your scope reaches"),
        }
    }
}

/// A turn's new mark, with the one it replaced in the same turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMarked {
    pub mark: ChatMark,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced: Option<ChatMark>,
}
