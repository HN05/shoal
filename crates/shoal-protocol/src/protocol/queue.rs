use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::queue_entry_ids::QueueEntryId;

use super::Lifetime;

/// Requests about the agent queue under `max_heavy_agents`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueMethod {
    /// Admit the launch of a new chat's agent now or queue it.
    QueueChat {
        request: Box<crate::model::ChatQueueRequest>,
    },
    /// Queue the worker of the ready `workspace`, the launch the daemon
    /// resolves `launch` into under the workspace's settings, on the
    /// workspace's issue unless the launch gives it a task, once `blockers`
    /// are done and it can run there, linking `pr` as it starts. The daemon
    /// starts it detached, beside the terminal the launch's `env` names or
    /// else in the default host, once the queue admits it. Answers with the
    /// entry.
    QueueAgent {
        workspace: SpaceSelector,
        launch: Box<super::operations::LaunchRequest>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blockers: Vec<crate::progress::Blocker>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr: Option<crate::forge::items::reference::ItemInput>,
    },
    ListQueue,
    /// The plan a queued start runs.
    QueuedPlan {
        entry: QueueEntryId,
    },
    /// Cancel a waiting entry, named by ID; a cancelled restore leaves its
    /// agent stopped.
    QueueRemove {
        entry: String,
    },
    /// Start a waiting entry, named by ID, now, regardless of the limits;
    /// with `here`, a launch the caller starts in its own process, which
    /// holds its slot.
    QueueStart {
        entry: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        here: bool,
    },
    /// Start the launch `entry`, which the caller held while its workspace
    /// was prepared, in the caller: at once when its start asked for that,
    /// else when the limits admit it now. One they do not admit stays
    /// waiting for the queue, which starts it once the caller lets go.
    /// Answers with the entry, starting or waiting.
    QueueClaim {
        entry: QueueEntryId,
    },
    /// The workspace a launch that left the queue started its agent in,
    /// while the daemon still knows it.
    StartedLaunch {
        entry: QueueEntryId,
    },
    /// Keep a starting entry's slot while the connection stays open.
    QueueHold {
        entry: QueueEntryId,
    },
    /// Record the workspace a starting entry opened, or the existing one a
    /// waiting launch opens, or the chat a launch starts its agent in.
    QueueAttach {
        entry: QueueEntryId,
        space: SpaceSelector,
    },
    /// End a starting entry that could not start its agent; a launch in a
    /// recorded space stays there as a failed entry.
    QueueFail {
        entry: QueueEntryId,
        error: String,
    },
    /// Queue the failed launch `entry` again exactly as it was, and start
    /// it as `start` says. Answers with the entry as it waits or starts.
    QueueRetry {
        entry: QueueEntryId,
        #[serde(default, skip_serializing_if = "RetryStart::is_queued")]
        start: RetryStart,
    },
    /// Set `priority` on the space, which ranks its queued work; a scoped
    /// caller gives at most its own space's.
    SetPriority {
        space: SpaceSelector,
        priority: shoal_schedule::priority::Priority,
    },
    /// Queue the restore of a stopped agent, which the daemon starts with
    /// `env` once admitted, or with `now` at once, regardless of the limits.
    QueueRestore {
        workspace: SpaceSelector,
        execution: ExecutionId,
        env: std::collections::BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        now: bool,
    },
}

/// How a failed launch queued again starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryStart {
    /// As the queue admits it.
    #[default]
    Queued,
    /// At once, regardless of the limits, which a scoped caller does as its
    /// `limits` permission allows.
    Now,
    /// In the caller, which holds it: the queue keeps it out of admission
    /// meanwhile.
    Here,
}

impl RetryStart {
    pub fn is_queued(&self) -> bool {
        *self == Self::Queued
    }
}

impl QueueMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::QueueHold { .. } => Lifetime::Resumed,
            _ => Lifetime::Reply,
        }
    }
}
