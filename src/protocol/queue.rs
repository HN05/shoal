use serde::{Deserialize, Serialize};

use super::Lifetime;

/// Requests about the agent queue under `max_agents`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueMethod {
    QueueAdd {
        request: crate::model::QueueRequest,
    },
    ListQueue,
    /// The plan a queued start runs.
    QueuedPlan {
        entry: String,
    },
    /// Remove a waiting entry, named by ID or workspace name.
    QueueRemove {
        entry: String,
    },
    /// Keep a starting entry's slot while the connection stays open.
    QueueHold {
        entry: String,
    },
    /// Record the workspace a starting entry opened.
    QueueAttach {
        entry: String,
        workspace: String,
    },
    /// End a starting entry that could not start its agent.
    QueueFail {
        entry: String,
        error: String,
    },
}

impl QueueMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::QueueHold { .. } => Lifetime::Resumed,
            _ => Lifetime::Reply,
        }
    }
}
