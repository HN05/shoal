//! What `shoal messages` prints.
use crate::space_names::SpaceName;
use serde::{Deserialize, Serialize};

use crate::output::outputs;

/// The spaces a message to every agent reached, by whether their agent
/// receives it now or once it next runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reached {
    /// The spaces whose agent runs.
    pub now: ReachedSpaces,
    /// The spaces whose agent is stopped, queued, waits for its restore or
    /// has a wrapper waiting to reattach.
    pub on_start: ReachedSpaces,
    /// The spaces that refused the message, such as one whose unread
    /// messages are full, which the others still received.
    #[serde(default, skip_serializing_if = "Refused::is_empty")]
    pub refused: Refused,
}

impl Reached {
    pub fn is_empty(&self) -> bool {
        self.now.is_empty() && self.on_start.is_empty() && self.refused.is_empty()
    }
}

/// Workspaces and chats that refused a message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub workspaces: Vec<RefusedSpace>,
    pub chats: Vec<RefusedSpace>,
}

impl Refused {
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty() && self.chats.is_empty()
    }
}

/// A space that refused a message, by name, with why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedSpace {
    pub name: SpaceName,
    pub error: String,
}

/// Workspaces and chats a message reached, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachedSpaces {
    pub workspaces: Vec<SpaceName>,
    pub chats: Vec<SpaceName>,
}

impl ReachedSpaces {
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty() && self.chats.is_empty()
    }
}

outputs!(Reached);
