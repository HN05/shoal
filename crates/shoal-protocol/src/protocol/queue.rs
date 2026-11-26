use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};
use shoal_ids::queue_entry_ids::QueueEntryId;

use super::{Forwarding, Lifetime, Target};

/// Requests about the agent queue under `max_heavy_agents`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueMethod {
    /// Queue the worker of the ready `workspace`, resolving `launch` under
    /// the workspace's settings. It waits for `blockers` and links `pr` as
    /// it starts. The daemon starts it detached once the queue admits it.
    /// Answers with the entry.
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
    /// Open the space of the starting launch `entry` for the agent its
    /// caller runs there now, as admission would for a queued agent. A
    /// workspace must still be ready, with its verified worktree. Answers
    /// with the plan, as [`Self::QueuedPlan`].
    OpenStart {
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
    /// Long-lived: admit a run of the `shoal run` command `command` in
    /// `space`, at the memory its earlier runs estimate, which it holds
    /// until the connection ends. The daemon answers, then sends
    /// [`AdmissionFrame`](crate::runs::AdmissionFrame)s and reads
    /// [`RunReport`](crate::runs::RunReport)s. `now` admits it at once,
    /// whatever the limits.
    AdmitCommand {
        space: SpaceSelector,
        command: crate::runs::CommandKey,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        now: bool,
        #[serde(default)]
        admit_as: crate::runs::AdmitAs,
    },
    /// Set `priority` on the space, which ranks its queued work; a scoped
    /// caller gives at most its own space's.
    SetPriority {
        space: SpaceSelector,
        priority: shoal_schedule::priority::Priority,
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
            Self::QueueAgent { .. }
            | Self::ListQueue
            | Self::QueuedPlan { .. }
            | Self::OpenStart { .. }
            | Self::QueueRemove { .. }
            | Self::QueueStart { .. }
            | Self::QueueClaim { .. }
            | Self::StartedLaunch { .. }
            | Self::QueueAttach { .. }
            | Self::QueueFail { .. }
            | Self::QueueRetry { .. }
            | Self::SetPriority { .. } => Lifetime::Reply,
            Self::AdmitCommand { .. } => Lifetime::Open,
        }
    }
}

impl QueueMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::ListQueue | Self::QueuedPlan { .. } => true,
            Self::QueueAgent { .. }
            | Self::OpenStart { .. }
            | Self::QueueRemove { .. }
            | Self::QueueStart { .. }
            | Self::QueueClaim { .. }
            | Self::StartedLaunch { .. }
            | Self::QueueHold { .. }
            | Self::QueueAttach { .. }
            | Self::QueueFail { .. }
            | Self::QueueRetry { .. }
            | Self::SetPriority { .. }
            | Self::AdmitCommand { .. } => false,
        }
    }
}

impl QueueMethod {
    /// A space's priority goes to its machine. An entry is named by an ID
    /// only its machine knows, so entry requests stay with the caller.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::SetPriority { .. } => Forwarding::Online,
            Self::QueueAgent { .. }
            | Self::ListQueue
            | Self::QueuedPlan { .. }
            | Self::OpenStart { .. }
            | Self::QueueRemove { .. }
            | Self::QueueStart { .. }
            | Self::QueueClaim { .. }
            | Self::StartedLaunch { .. }
            | Self::QueueHold { .. }
            | Self::QueueAttach { .. }
            | Self::QueueFail { .. }
            | Self::QueueRetry { .. }
            | Self::AdmitCommand { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::QueueAgent { workspace, .. } => Some(Target::Space(workspace)),
            Self::SetPriority { space, .. } | Self::AdmitCommand { space, .. } => {
                Some(Target::Space(space))
            }
            // It names the space its entry's start opened.
            Self::QueueAttach { .. }
            | Self::ListQueue
            | Self::QueuedPlan { .. }
            | Self::OpenStart { .. }
            | Self::QueueRemove { .. }
            | Self::QueueStart { .. }
            | Self::QueueClaim { .. }
            | Self::StartedLaunch { .. }
            | Self::QueueHold { .. }
            | Self::QueueFail { .. }
            | Self::QueueRetry { .. } => None,
        }
    }
}
