use crate::client::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

use super::Lifetime;

/// Requests about the agent queue under `max_agents`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueMethod {
    QueueAdd {
        request: crate::model::QueueRequest,
    },
    /// Queue `agent` in the ready `workspace`, with `prompt` as its first
    /// message, once it can run there; the daemon starts it detached in the
    /// default host once the queue admits it. Answers with the entry.
    QueueAgent {
        workspace: SpaceSelector,
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
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
    /// Move a waiting entry to `place`, or stop a running agent and place
    /// its restore there.
    QueueMove {
        entry: String,
        place: crate::model::QueuePlace,
    },
    /// Keep a starting entry's slot while the connection stays open.
    QueueHold {
        entry: String,
    },
    /// Record the workspace a starting entry opened, or the existing one a
    /// waiting launch opens.
    QueueAttach {
        entry: String,
        workspace: SpaceSelector,
    },
    /// End a starting entry that could not start its agent.
    QueueFail {
        entry: String,
        error: String,
    },
    /// Queue the restore of a stopped agent, which the daemon starts with
    /// `env` once admitted, or with `now` at once, regardless of the limits.
    QueueRestore {
        workspace: SpaceSelector,
        execution: String,
        env: std::collections::BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        now: bool,
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
