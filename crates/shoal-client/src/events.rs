//! The workspace and chat event journal, as `shoal events` replays it.
use crate::execution_ids::ExecutionId;
use crate::repository_ids::RepositoryId;
use crate::space_ids::SpaceId;
use crate::space_names::SpaceName;
use std::{fmt, path::PathBuf, str::FromStr};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{io::BufReader, net::UnixStream};

use crate::{
    connection::{Daemon, open_buffered, reconnect, stream_closed},
    protocol::{self, Body, Response, WorkspaceMethod},
    state::states,
    time::UnixTime,
};

/// An event's ID in the journal, where a replay after it resumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventCursor(i64);

impl EventCursor {
    pub const fn new(id: i64) -> Self {
        Self(id)
    }

    pub const fn id(self) -> i64 {
        self.0
    }

    /// The cursor a replay of this event and those after it starts from.
    pub const fn before(self) -> Self {
        Self(self.0 - 1)
    }

    /// The event a replay from this cursor reads first.
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for EventCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A nonnegative event ID, as the command line passes a cursor.
impl FromStr for EventCursor {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let id: i64 = text.parse().context("an event cursor is an event ID")?;
        anyhow::ensure!(id >= 0, "an event cursor is nonnegative");
        Ok(Self(id))
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::ToSql for EventCursor {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::FromSql for EventCursor {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        i64::column_result(value).map(Self)
    }
}

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
    /// A chat opened the workspace, or it returned to the user; earlier
    /// versions recorded it when they moved the children of a removed
    /// workspace up to its own parent.
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
    /// Recorded only by earlier versions, when a chat opened the workspace
    /// or it returned to the user, as `parent_changed` records it now.
    ChatChanged => "chat_changed",
    /// A demo's command ran for the first time.
    DemoStarted => "demo_started",
    /// A demo that had run ended with its execution.
    DemoStopped => "demo_stopped",
    /// A simulator lease's device booted for the workspace.
    SimulatorAcquired => "simulator_acquired",
    /// A booted simulator lease was released or its device deleted.
    SimulatorReleased => "simulator_released",
    /// The workspace took a new name and branch, or a new `title`.
    Renamed => "renamed",
    /// `shoal restore` reopened the removed workspace, whose events
    /// the reopened one took.
    Restored => "restored",
    /// The space's agent started, or started again after a stop.
    AgentStarted => "agent_started",
    /// Shoal or a stop request stopped the space's agent.
    AgentStopped => "agent_stopped",
    /// The space's agent exited on its own.
    AgentExited => "agent_exited",
    /// The space's agent asked a question.
    QuestionAsked => "question_asked",
    /// A question the space asked was answered.
    QuestionAnswered => "question_answered",
    /// The space asked for an approval.
    ApprovalRequested => "approval_requested",
    /// An approval the space asked for was approved or denied.
    ApprovalDecided => "approval_decided",
    /// The space's agent sent an alert.
    AlertSent => "alert_sent",
    /// The space's agent changed its status line or plan steps.
    ProgressChanged => "progress_changed",
    /// A space was created below the space, which `child` names.
    ChildCreated => "child_created",
    /// A space below the space was removed.
    ChildRemoved => "child_removed",
});

states!(
    /// What happened to a chat.
    ChatEventKind {
        Started => "chat_started",
        /// The chat was removed.
        Ended => "chat_ended",
        /// The chat took a new name, which its `chat` carries.
        Renamed => "chat_renamed",
        /// The chat stopped, as Shoal saved its session or its agent
        /// exited, to restore as its `chat`'s `restore` says.
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

/// An event of a space, a workspace or a chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDetails {
    pub kind: EventKind,
    /// Null for a chat's event.
    pub workspace_id: Option<SpaceId>,
    /// Null for a workspace's event.
    #[serde(default)]
    pub chat_id: Option<SpaceId>,
    /// Null for a chat without a repository.
    pub repository_id: Option<RepositoryId>,
    pub name: SpaceName,
    /// Null for a chat.
    pub path: Option<PathBuf>,
    /// Observed branch for branch changes; null for a detached HEAD.
    pub branch: Option<String>,
    pub cause: Option<EventCause>,
    pub error: Option<String>,
    /// The ref a workspace started from, such as `refs/heads/main`, on its
    /// creation; null for one at a commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
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
    pub parent: Option<Option<crate::model::SpaceRef>>,
    /// The parent workspace on creation and parent changes recorded by
    /// earlier versions, which `parent` replaces.
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
    /// Present on chat changes recorded by earlier versions; null once the
    /// workspace returned to the user.
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
    /// The workspace's new title; null once it has none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "present_or_null"
    )]
    pub title: Option<Option<String>>,
    /// The agent that started, stopped or exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentEvent>,
    /// The question asked or answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<Box<QuestionEvent>>,
    /// The approval requested or decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<Box<ApprovalEvent>>,
    /// The alert sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert: Option<AlertEvent>,
    /// The space's progress after a change, as `shoal progress --json`
    /// shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<crate::progress::Progress>,
    /// The space created or removed below this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<crate::model::SpaceRef>,
}

impl EventDetails {
    /// The workspace or chat the event belongs to.
    pub fn space_id(&self) -> Option<&SpaceId> {
        self.workspace_id.as_ref().or(self.chat_id.as_ref())
    }
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
pub struct AgentEvent {
    /// The agent's name, such as `claude`.
    pub name: crate::agent::Agent,
    /// The execution that runs it; null for a chat's agent.
    pub execution_id: Option<ExecutionId>,
    /// The code an agent exited with; null when its wrapper disconnected
    /// without reporting one, and for a start or stop.
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionEvent {
    #[serde(flatten)]
    pub number: crate::item_numbers::ItemNumber,
    pub question: String,
    /// Null when it was asked.
    pub answer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalEvent {
    #[serde(flatten)]
    pub number: crate::item_numbers::ItemNumber,
    /// What the request is for, such as `port/web` or `pr_open`.
    pub target: String,
    /// What approving it allows, as `shoal approvals` describes it.
    pub subject: String,
    pub status: crate::approvals::DecisionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertEvent {
    pub message: String,
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
    pub id: EventCursor,
    pub created_at: UnixTime,
    #[serde(flatten)]
    pub details: EventDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatEvent {
    pub id: EventCursor,
    pub created_at: UnixTime,
    pub kind: ChatEventKind,
    pub chat: crate::model::Chat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEvent {
    pub id: EventCursor,
    pub created_at: UnixTime,
    #[serde(flatten)]
    pub details: FileEventDetails,
}

/// A file event as stored: the space is a workspace or a chat, which may be
/// gone, so it carries the space's identity in place of its record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEventDetails {
    pub kind: FileEventKind,
    /// Null for a chat's file.
    pub workspace_id: Option<SpaceId>,
    /// Null for a workspace's file.
    pub chat_id: Option<SpaceId>,
    /// Null for the file of a chat without a repository.
    pub repository_id: Option<RepositoryId>,
    /// The record as `shoal files` lists it.
    pub file: crate::files::AttachedFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventItem {
    Event(Box<WorkspaceEvent>),
    Chat(Box<ChatEvent>),
    File(Box<FileEvent>),
    /// The journal no longer holds every event after the requested cursor.
    Gap {
        #[serde(rename = "since")]
        after: EventCursor,
        oldest_id: EventCursor,
        latest_id: EventCursor,
    },
}

impl EventItem {
    /// The journal ID of an event; a gap has none.
    pub fn event_id(&self) -> Option<EventCursor> {
        match self {
            Self::Event(event) => Some(event.id),
            Self::Chat(event) => Some(event.id),
            Self::File(event) => Some(event.id),
            Self::Gap { .. } => None,
        }
    }

    /// Where a replay resumes after this item: the event, or for a gap the
    /// start of the retained journal.
    pub fn cursor(&self) -> EventCursor {
        match self {
            Self::Event(event) => event.id,
            Self::Chat(event) => event.id,
            Self::File(event) => event.id,
            Self::Gap { oldest_id, .. } => oldest_id.before(),
        }
    }
}

/// The journal of workspace, chat and file events from a cursor: a replay
/// of the retained events, then with `follow` each new one as it happens,
/// across daemon restarts.
pub struct Events {
    reader: BufReader<UnixStream>,
    follow: bool,
    cursor: Option<EventCursor>,
}

impl Events {
    /// The events after `after`, or every retained one.
    pub async fn open(
        daemon: &impl Daemon,
        after: Option<EventCursor>,
        follow: bool,
    ) -> Result<Self> {
        let method = WorkspaceMethod::WatchWorkspaceEvents { after, follow };
        let (reader, body) = open_buffered(daemon, method).await?;
        <()>::try_from(body)?;
        Ok(Self {
            reader,
            follow,
            cursor: after,
        })
    }

    /// Where the stream resumes: after the last item it returned.
    pub fn cursor(&self) -> Option<EventCursor> {
        self.cursor
    }

    /// The next item; `None` once a replay without `follow` ends.
    pub async fn next(&mut self, daemon: &impl Daemon) -> Result<Option<EventItem>> {
        loop {
            let response: Response = match protocol::read_buffered(&mut self.reader).await {
                Ok(response) => response,
                Err(error) if self.follow && stream_closed(&error) => {
                    let (after, follow) = (self.cursor, self.follow);
                    let method = || WorkspaceMethod::WatchWorkspaceEvents { after, follow };
                    let (reader, body) = reconnect(daemon, method).await?;
                    <()>::try_from(body)?;
                    self.reader = reader;
                    continue;
                }
                Err(error) => return Err(error).context("daemon closed the event stream"),
            };
            let item = match response.body {
                Body::Ok if !self.follow => return Ok(None),
                Body::EventItem(item) => item,
                body => return Err(body.unexpected("EventItem")),
            };
            self.cursor = Some(item.cursor());
            return Ok(Some(item));
        }
    }
}

#[cfg(test)]
mod tests;
