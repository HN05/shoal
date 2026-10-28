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
    /// Cancel a waiting entry, named by ID or workspace name; a cancelled
    /// restore leaves its agent stopped.
    QueueRemove {
        entry: String,
    },
    /// Start a waiting entry now, regardless of the limits.
    QueueStart {
        entry: String,
    },
    /// Move a waiting entry to `place`.
    QueueMove {
        entry: String,
        place: crate::model::QueuePlace,
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
    /// Queue the restore of a stopped agent, which the daemon starts with
    /// `env` once admitted.
    QueueRestore {
        workspace: String,
        execution: String,
        env: std::collections::BTreeMap<String, String>,
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
