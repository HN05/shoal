//! The records subscriptions carry: repositories, workspaces with their
//! agents, the issues, PRs and CI runs they link with their forge state,
//! the sessions each space's agents reported, open questions, pending
//! approval requests, messages and notifications, each named by a
//! [`RecordKey`].
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    approvals::ApprovalRequest,
    forge::{ci::check_state::CheckState, items::link::LinkKind, updates::SnapshotState},
    model::{Relation, Repository, Workspace},
    notifications::Notification,
    questions::Question,
    state::states,
    transcripts::ReportedSession,
};

states!(
    /// What a subscription covers.
    Topic: Variants {
        Repositories => "repositories",
        /// Workspaces with their lifecycle state, base workspace, holds,
        /// ready-for-review marks and agent.
        Workspaces => "workspaces",
        /// The issues, PRs and CI runs workspaces link, with their forge state.
        Links => "links",
        /// The sessions each workspace's and chat's agents reported.
        Sessions => "sessions",
        /// Open questions.
        Questions => "questions",
        /// Pending approval requests.
        Approvals => "approvals",
        /// Messages to agents, and from agents to the user.
        Messages => "messages",
        /// The user's notifications; workspace processes cannot subscribe.
        Notifications => "notifications",
    }
);

states!(
    /// The kinds of record.
    #[derive(PartialOrd, Ord)]
    RecordKind {
        Repository => "repository",
        Workspace => "workspace",
        Link => "link",
        Sessions => "sessions",
        Question => "question",
        Approval => "approval",
        Message => "message",
        Notification => "notification",
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
    Workspace(Box<WorkspaceRecord>),
    Link(Box<Link>),
    Sessions(ReportedSessions),
    Question(Question),
    Approval(Box<ApprovalRequest>),
    Message(Message),
    Notification(Notification),
    /// An entry of the space's log the subscription follows.
    LogEntry(Box<crate::work::LogEntry>),
}

impl Record {
    pub fn key(&self) -> RecordKey {
        let (kind, id) = match self {
            Self::Repository(record) => (RecordKind::Repository, record.id.clone()),
            Self::Workspace(record) => (RecordKind::Workspace, record.workspace.id.clone()),
            Self::Link(record) => (RecordKind::Link, record.id.clone()),
            Self::Sessions(record) => (RecordKind::Sessions, record.space_id.clone()),
            Self::Question(record) => (RecordKind::Question, record.number.to_string()),
            Self::Approval(record) => (RecordKind::Approval, record.key.clone()),
            Self::Message(record) => (RecordKind::Message, record.id.clone()),
            Self::Notification(record) => (RecordKind::Notification, record.id.to_string()),
            // Padded, so keys sort as the log does.
            Self::LogEntry(record) => (RecordKind::LogEntry, format!("{:020}", record.id())),
        };
        RecordKey { kind, id }
    }
}

/// A workspace, with its agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub workspace: Workspace,
    /// Its agent, when one runs, waits in the queue or stopped with a
    /// session to restore.
    pub agent: Option<AgentActivity>,
}

states!(
    /// What a workspace's agent is doing.
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
        /// Stopped with a session to restore.
        Stopped => "stopped",
        /// Its execution has not reported back since a daemon restart, so
        /// whether it still runs is unconfirmed.
        Unconfirmed => "unconfirmed",
    }
);

/// A workspace's agent and what it is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActivity {
    pub state: ActivityState,
    /// The agent's name, such as `claude`, when known.
    pub agent: Option<String>,
    /// When the state was reported, when known.
    pub since: Option<crate::time::UnixTime>,
}

/// An issue, PR or CI run a workspace links, with its state as the daemon
/// last recorded it from the forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub id: String,
    pub workspace_id: String,
    pub kind: LinkKind,
    pub url: String,
    pub relation: Relation,
    pub title: Option<String>,
    /// Absent until the daemon has looked.
    pub state: Option<SnapshotState>,
    /// A PR's head commit, which its checks ran on.
    pub head: Option<String>,
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
    pub space_id: String,
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

states!(
    /// Who a message is for.
    MessageRecipient {
        /// A workspace's agent, until it is delivered.
        Agent => "agent",
        /// The user, from a workspace process.
        User => "user",
    }
);

/// A message to a workspace's agent, or from one to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub recipient: MessageRecipient,
    /// The workspace's ID, for a message to its agent.
    pub workspace_id: Option<String>,
    /// The workspace's name.
    pub workspace: Option<String>,
    /// Empty when the message is only its attachments.
    pub text: String,
    /// Shoal's copies of the attached files.
    pub attachments: Vec<PathBuf>,
    pub created_at: crate::time::UnixTime,
}
