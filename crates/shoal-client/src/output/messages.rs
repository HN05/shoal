//! What `shoal messages` prints.
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
}

impl Reached {
    pub fn is_empty(&self) -> bool {
        self.now.is_empty() && self.on_start.is_empty()
    }
}

/// Workspaces and chats a message reached, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachedSpaces {
    pub workspaces: Vec<String>,
    pub chats: Vec<String>,
}

impl ReachedSpaces {
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty() && self.chats.is_empty()
    }
}

outputs!(Reached);
