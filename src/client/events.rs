//! The workspace and chat event journal, as `shoal events` replays it.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::state::states;

states!(EventKind {
    Created => "created",
    Ready => "ready",
    SetupFailed => "setup_failed",
    Completed => "completed",
    /// Recorded only by earlier versions, before `undone` replaced continuation.
    Continued => "continued",
    /// A recorded completion was withdrawn.
    Undone => "undone",
    Removed => "removed",
    Retained => "retained",
    BranchChanged => "branch_changed",
    /// A workspace process marked linked work ready for review.
    ReviewReady => "review_ready",
    /// A ready mark was withdrawn or its item unlinked.
    ReviewCleared => "review_cleared",
    /// The workspace's base workspace was set, cleared, or removed.
    BaseChanged => "base_changed",
    /// Recorded only by earlier versions, which moved the children of a
    /// removed workspace up to its own parent.
    ParentChanged => "parent_changed",
    /// A workspace link was added.
    Linked => "linked",
    /// A workspace link was removed.
    Unlinked => "unlinked",
    /// The workspace's agent reported a turn state, or its execution ended.
    AgentState => "agent_state",
    /// Shoal changed an issue or PR for the workspace.
    ItemChanged => "item_changed",
    /// An issue or PR changed on its forge.
    ItemActivity => "item_activity",
    /// A chat opened the workspace, or it returned to the user.
    ChatChanged => "chat_changed",
    /// A demo's command ran for the first time.
    DemoStarted => "demo_started",
    /// A demo that had run ended with its execution.
    DemoStopped => "demo_stopped",
    /// A simulator lease's device booted for the workspace.
    SimulatorAcquired => "simulator_acquired",
    /// A booted simulator lease was released or its device deleted.
    SimulatorReleased => "simulator_released",
});

states!(
    /// What happened to a chat.
    ChatEventKind {
        Started => "chat_started",
        Ended => "chat_ended",
        /// The chat took a new name, which its `chat` carries.
        Renamed => "chat_renamed",
        /// Shoal stopped the chat with its session saved, to restore as
        /// its `chat`'s `restore` says.
        Saved => "chat_saved",
        /// A saved chat runs again.
        Restored => "chat_restored",
    }
);

states!(
    /// What happened to a file attached to a workspace or chat.
    FileEventKind {
        Added => "file_added",
        /// Its record and Shoal's copy went, by whatever deleted them.
        Removed => "file_removed",
    }
);

states!(EventCause {
    Manual => "manual",
    Idle => "idle",
    Issue => "issue",
    Pr => "pr",
    Completion => "completion",
    MissingDirectory => "missing_directory",
    /// Free disk space fell below the configured cleanup threshold.
    DiskSpace => "disk_space",
    /// The branch merged into the default branch and the workspace stayed idle.
    Merged => "merged",
    /// The base workspace was removed and its own took its place, or, in
    /// earlier versions, the parent workspace.
    Removed => "removed",
    /// The workspace's parent was removed, which takes its children first.
    Parent => "parent",
    /// An ancestor of the workspace finished its assignment with cleanup.
    ParentCompletion => "parent_completion",
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDetails {
    pub kind: EventKind,
    pub workspace_id: String,
    pub repository_id: String,
    pub name: String,
    pub path: PathBuf,
    /// Observed branch for branch changes; null for a detached HEAD.
    pub branch: Option<String>,
    pub cause: Option<EventCause>,
    pub error: Option<String>,
    /// The mark a review event describes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewEvent>,
    /// Present on creation and base changes; null when there is no base.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "present_or_null"
    )]
    pub base_workspace: Option<Option<crate::model::WorkspaceRef>>,
    /// Present on creation and parent changes; null when there is no parent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "present_or_null"
    )]
    pub parent_workspace: Option<Option<crate::model::WorkspaceRef>>,
    /// The issue, PR or CI run link changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Box<LinkEvent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<AgentStateEvent>,
    /// The issue or PR Shoal changed, and how.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Box<ItemEvent>>,
    /// An issue or PR changed on its forge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_activity: Option<Box<ItemActivityEvent>>,
    /// Present on chat changes; null once the workspace returned to the user.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "present_or_null"
    )]
    pub chat: Option<Option<String>>,
    /// The demo that started or stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demo: Option<DemoEvent>,
    /// The simulator lease acquired or released.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulator: Option<SimulatorEvent>,
}

/// Distinguish an absent field from an explicit null.
mod present_or_null {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(
        value: &Option<Option<T>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .as_ref()
            .and_then(Option::as_ref)
            .serialize(serializer)
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::deserialize(deserializer).map(Some)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewEvent {
    /// The linked issue or PR; both are null for a workspace mark.
    pub kind: Option<crate::forge::items::link::ItemKind>,
    pub url: Option<String>,
    pub head: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemoEvent {
    pub name: String,
    pub preset: Option<crate::demos::Preset>,
    /// The leased simulator a simulator demo runs on.
    pub udid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatorEvent {
    /// The lease's name, such as `default`.
    pub name: String,
    pub udid: Option<String>,
    pub device: String,
    pub runtime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStateEvent {
    /// Null once the execution that reported the state has ended.
    pub state: Option<crate::state::AgentState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkEvent {
    pub kind: crate::forge::items::link::LinkKind,
    pub url: String,
    /// Absent from events recorded before relations existed, which were assignments.
    #[serde(default)]
    pub relation: crate::model::Relation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemEvent {
    pub kind: crate::forge::items::link::LinkKind,
    pub url: String,
    /// The action's name, such as `edit` or `merge`.
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemActivityEvent {
    pub kind: crate::forge::items::link::LinkKind,
    pub url: String,
    pub update: ItemActivityUpdate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemActivityUpdate {
    pub kind: crate::forge::updates::UpdateKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceEvent {
    pub id: i64,
    pub created_at: i64,
    #[serde(flatten)]
    pub details: EventDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatEvent {
    pub id: i64,
    pub created_at: i64,
    pub kind: ChatEventKind,
    pub chat: crate::model::Chat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEvent {
    pub id: i64,
    pub created_at: i64,
    #[serde(flatten)]
    pub details: FileEventDetails,
}

/// A file event as stored: the space is a workspace or a chat, which may be
/// gone, so it carries the space's identity in place of its record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEventDetails {
    pub kind: FileEventKind,
    /// Null for a chat's file.
    pub workspace_id: Option<String>,
    /// Null for a workspace's file.
    pub chat_id: Option<String>,
    pub repository_id: String,
    /// The record as `shoal files` lists it.
    pub file: crate::client::files::AttachedFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventItem {
    Event(Box<WorkspaceEvent>),
    Chat(Box<ChatEvent>),
    File(Box<FileEvent>),
    Gap {
        since: i64,
        oldest_id: i64,
        latest_id: i64,
    },
}

impl EventItem {
    /// The journal ID of an event; a gap has none.
    pub fn event_id(&self) -> Option<i64> {
        match self {
            Self::Event(event) => Some(event.id),
            Self::Chat(event) => Some(event.id),
            Self::File(event) => Some(event.id),
            Self::Gap { .. } => None,
        }
    }

    /// Where a replay resumes after this item: the event, or for a gap the
    /// start of the retained journal.
    pub fn cursor(&self) -> i64 {
        match self {
            Self::Event(event) => event.id,
            Self::Chat(event) => event.id,
            Self::File(event) => event.id,
            Self::Gap { oldest_id, .. } => oldest_id - 1,
        }
    }
}
