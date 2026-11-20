//! The records subscriptions carry: repositories, spaces with their agents,
//! the issues, PRs and CI runs they link with their forge state,
//! the sessions each space's agents reported, open threads, pending
//! approval requests, messages and notifications, each named by a
//! [`RecordKey`].
use shoal_ids::space_ids::SpaceId;

use serde::{Deserialize, Serialize};
use shoal_refs::CommitId;
use shoal_states::states;

use crate::{
    alerts::Alert,
    approvals::ApprovalRequest,
    forge::{ci::check_state::CheckState, items::link::LinkKind, updates::SnapshotState},
    model::{Relation, Repository, Space},
    threads::Thread,
    transcripts::ReportedSession,
};

states!(
    /// What a subscription covers.
    Topic: Variants {
        Repositories => "repositories",
        /// Workspaces and chats with their state, holds, ready-for-review
        /// marks and agent.
        Spaces => "spaces",
        /// The issues, PRs and CI runs spaces link, with their forge state.
        Links => "links",
        /// The sessions each workspace's and chat's agents reported.
        Sessions => "sessions",
        /// Open threads.
        Threads => "threads",
        /// Pending approval requests.
        Approvals => "approvals",
        /// Unread alerts to the agents of the spaces, and the user's alerts,
        /// which scoped callers do not receive.
        Alerts => "alerts",
    }
);

states!(
    /// The kinds of record.
    #[derive(PartialOrd, Ord)]
    RecordKind {
        Repository => "repository",
        Space => "space",
        Link => "link",
        Sessions => "sessions",
        Thread => "thread",
        Approval => "approval",
        Alert => "alert",
        LogEntry => "log_entry",
    }
);

/// What names a record among those of its kind. The ID is opaque: compare
/// it, never parse it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RecordKey {
    pub kind: RecordKind,
    pub id: String,
}

/// One record, tagged with its kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "record", rename_all = "snake_case")]
pub enum Record {
    Repository(Repository),
    Space(Box<SpaceRecord>),
    Link(Box<Link>),
    Sessions(ReportedSessions),
    Thread(Box<Thread>),
    Approval(Box<ApprovalRequest>),
    Alert(Alert),
    /// An entry of the space's log the subscription follows.
    LogEntry(Box<crate::log::LogEntry>),
}

impl Record {
    pub fn key(&self) -> RecordKey {
        let (kind, id) = match self {
            Self::Repository(record) => (RecordKind::Repository, record.id.to_string()),
            Self::Space(record) => (RecordKind::Space, record.space.id.to_string()),
            Self::Link(record) => (RecordKind::Link, record.id.clone()),
            Self::Sessions(record) => (RecordKind::Sessions, record.space_id.to_string()),
            Self::Thread(record) => (RecordKind::Thread, record.number.to_string()),
            Self::Approval(record) => (RecordKind::Approval, record.key.clone()),
            Self::Alert(record) => (RecordKind::Alert, record.id.to_string()),
            // Padded, so keys sort as the log does.
            Self::LogEntry(record) => (RecordKind::LogEntry, format!("{:020}", record.id())),
        };
        RecordKey { kind, id }
    }
}

/// A space, with its agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceRecord {
    /// A workspace's record carries its worktree, a chat's its own part.
    pub space: Space,
    /// Its agent, when one runs, waits in the queue or stopped with a
    /// session to restore.
    pub agent: Option<AgentActivity>,
}

states!(
    /// What a space's agent is doing.
    ActivityState {
        /// Running and working on a turn.
        Working => "working",
        /// Running and stopped mid-turn for the user, such as at a permission prompt.
        Waiting => "waiting",
        /// Running with its turn finished.
        Idle => "idle",
        /// Running without a reported turn state.
        Running => "running",
        /// Waiting in the agent queue for a slot or for work it depends on.
        Queued => "queued",
        /// Admitted from the queue; it has not started yet.
        Starting => "starting",
        /// Its launch failed to start; `shoal resume` queues it again.
        Failed => "failed",
        /// Stopped with a session to restore.
        Stopped => "stopped",
        /// Its execution has not reported back since a daemon restart, so
        /// whether it still runs is unconfirmed.
        Unconfirmed => "unconfirmed",
    }
);

/// A space's agent and what it is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActivity {
    pub state: ActivityState,
    /// The agent's name, such as `claude`, when known.
    pub agent: Option<crate::agent::Agent>,
    /// When the state was reported, when known.
    pub since: Option<crate::time::UnixTime>,
}

/// An issue, PR or CI run a space links, with its state as the daemon last
/// recorded it from the forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub id: String,
    pub space_id: SpaceId,
    pub kind: LinkKind,
    pub url: crate::forge::items::reference::ItemRef,
    pub relation: Relation,
    pub title: Option<String>,
    /// Absent until the daemon has looked.
    pub state: Option<SnapshotState>,
    /// A PR's head commit, which its checks ran on.
    pub head: Option<CommitId>,
    /// A PR's CI checks on its head.
    pub checks: Vec<Check>,
    /// Whether a PR's head conflicts with its base; absent when unknown,
    /// such as while the forge has not checked a new head or base.
    pub conflict: Option<bool>,
    /// What the daemon could not read when it last looked, such as CI
    /// checks; what it covers may be older than the rest.
    pub lookup_errors: Vec<LookupError>,
}

/// The sessions a workspace's or chat's agents reported, whose files
/// [`crate::transcripts`] finds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedSessions {
    pub space_id: SpaceId,
    pub sessions: Vec<ReportedSession>,
}

/// One CI check of a PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    /// Whether the check finished.
    pub complete: bool,
}

/// A part of an item the daemon could not read from its forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupError {
    /// What it could not read, such as `CI`.
    pub source: String,
    pub message: String,
}
