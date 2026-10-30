//! The records subscriptions carry and calls return: repositories,
//! workspaces, their links, questions, approvals, messages and
//! notifications, each named by a [`RecordKey`].
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::open_enum::open_enum;

open_enum! {
    /// The kinds of record.
    RecordKind {
        Repository => "repository",
        Workspace => "workspace",
        Link => "link",
        Question => "question",
        Approval => "approval",
        Message => "message",
        Notification => "notification",
    }
}

/// What names a record among those of its kind. The ID is opaque: compare
/// it, never parse it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct RecordKey {
    #[serde(rename = "type")]
    pub kind: RecordKind,
    pub id: String,
}

/// One record, tagged with its kind as `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Repository(Repository),
    Workspace(Box<Workspace>),
    Link(Box<Link>),
    Question(Question),
    Approval(Approval),
    Message(Message),
    Notification(Notification),
    /// A kind this version does not know; ignore it.
    #[serde(other)]
    Unknown,
}

impl Record {
    /// The record's key; `None` for a kind this version does not know.
    pub fn key(&self) -> Option<RecordKey> {
        let (kind, id) = match self {
            Self::Repository(record) => (RecordKind::Repository, record.id.clone()),
            Self::Workspace(record) => (RecordKind::Workspace, record.id.clone()),
            Self::Link(record) => (RecordKind::Link, record.id.clone()),
            Self::Question(record) => (RecordKind::Question, record.id.to_string()),
            Self::Approval(record) => (RecordKind::Approval, record.id.to_string()),
            Self::Message(record) => (RecordKind::Message, record.id.clone()),
            Self::Notification(record) => (RecordKind::Notification, record.id.to_string()),
            Self::Unknown => return None,
        };
        Some(RecordKey { kind, id })
    }
}

/// A repository registered with Shoal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Repository {
    pub id: String,
    /// The name it was registered under, if any.
    pub name: Option<String>,
    /// Its registered checkout.
    pub path: PathBuf,
    /// The path or URL it was registered from.
    pub source: String,
}

open_enum! {
    /// Where a workspace is in its lifecycle.
    WorkspaceState {
        Preparing => "preparing",
        Ready => "ready",
        Stopping => "stopping",
        Removing => "removing",
        Reconciling => "reconciling",
        /// Returning to a checkpoint.
        Restoring => "restoring",
        /// Fast-forwarding to its branch's pushed head.
        Updating => "updating",
        Failed => "failed",
    }
}

/// A workspace: a worktree on its own branch, with what Shoal records about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Workspace {
    pub id: String,
    pub repository_id: String,
    pub name: String,
    /// What its agent calls the work.
    pub title: Option<String>,
    pub path: PathBuf,
    pub branch: String,
    pub state: WorkspaceState,
    /// Why the workspace failed or needs repair.
    pub error: Option<String>,
    /// Its assignment is done and the completion not withdrawn.
    pub done: bool,
    /// The workspace whose branch this one builds on.
    pub base_workspace: Option<WorkspaceRef>,
    /// The workspace this one was split from.
    pub parent_workspace: Option<WorkspaceRef>,
    /// Holds that keep it from cleanup.
    pub holds: Vec<Hold>,
    /// Marks that its work is ready for review.
    pub ready_marks: Vec<ReadyMark>,
    /// Its agent, when one runs, waits in the queue or stopped with a session to restore.
    pub agent: Option<AgentStatus>,
}

/// Another workspace a record refers to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceRef {
    pub id: String,
    pub name: String,
    pub branch: String,
}

/// A named claim that a workspace is still in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Hold {
    pub name: String,
    pub reason: Option<String>,
    /// Unix seconds.
    pub created_at: i64,
}

/// A statement that linked work is ready for review at a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadyMark {
    /// The marked issue or PR; absent when the mark covers the workspace.
    pub item: Option<Item>,
    /// The commit the mark covers.
    pub head: String,
    /// Unix seconds.
    pub created_at: i64,
}

/// An issue or PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Item {
    pub kind: ItemKind,
    pub url: String,
}

open_enum! {
    /// What a workspace links.
    ItemKind {
        Issue => "issue",
        Pr => "pr",
        /// A CI workflow run.
        Ci => "ci",
    }
}

open_enum! {
    /// What a workspace's agent is doing.
    AgentState {
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
    }
}

/// A workspace's agent and its state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentStatus {
    pub state: AgentState,
    /// The agent's name, such as `claude`, when known.
    pub agent: Option<String>,
    /// Unix seconds since the state was reported, when known.
    pub since: Option<i64>,
}

open_enum! {
    /// How an issue or PR relates to a workspace.
    Relation {
        /// The work the workspace exists for.
        Assigned => "assigned",
        /// The workspace created it through Shoal.
        Opened => "opened",
        /// It informs the work without driving it.
        Related => "related",
    }
}

/// An issue or PR a workspace links, with its state as the daemon last
/// recorded it from the forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Link {
    pub id: String,
    pub workspace_id: String,
    pub kind: ItemKind,
    pub url: String,
    pub relation: Relation,
    pub title: Option<String>,
    /// The forge's state, such as `open`, `closed` or `merged`; absent until
    /// the daemon has looked.
    pub state: Option<String>,
    /// A PR's head commit, which its checks ran on.
    pub head: Option<String>,
    /// A PR's CI checks on its head.
    pub checks: Vec<Check>,
    /// Whether a PR conflicts with its base; absent when unknown.
    pub conflict: Option<bool>,
}

/// One CI check of a PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Check {
    pub name: String,
    /// The forge's state, such as `success`, `failure` or `pending`.
    pub state: String,
    /// Whether the check finished.
    pub complete: bool,
}

/// An open question a workspace's agent asked the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Question {
    /// The number it is answered by; reused once the question is gone.
    pub id: i64,
    pub workspace_id: String,
    /// The workspace's name.
    pub workspace: String,
    pub question: String,
    pub recommendation: String,
    pub reason: Option<String>,
    pub choices: Vec<String>,
    /// Unix seconds.
    pub created_at: i64,
}

open_enum! {
    ApprovalStatus {
        Pending => "pending",
        Approved => "approved",
        Denied => "denied",
        Outdated => "outdated",
        /// The requesting agent no longer needed it.
        Withdrawn => "withdrawn",
    }
}

/// A workspace's request for the user's approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Approval {
    /// The number it is decided by; reused once the request is gone.
    pub id: i64,
    /// What names this request for good, unlike its number.
    pub key: String,
    pub workspace_id: String,
    /// The workspace's name.
    pub workspace: String,
    /// What it authorizes, such as `simulator`, `port/web` or `pr_open`.
    pub target: String,
    pub name: String,
    pub reason: String,
    pub status: ApprovalStatus,
    /// Unix seconds.
    pub created_at: u64,
}

open_enum! {
    /// Who a message is for.
    MessageRecipient {
        /// A workspace's agent, until it is delivered.
        Agent => "agent",
        /// The user, from a workspace process.
        User => "user",
    }
}

/// A message to a workspace's agent or from one to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// Unix seconds.
    pub created_at: i64,
}

/// Something the daemon tells the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Notification {
    pub id: i64,
    /// What happened, such as `question_asked` or `workspace_removed`.
    pub kind: String,
    /// The workspace's name when it happened.
    pub workspace: Option<String>,
    pub message: String,
    pub read: bool,
    /// Unix seconds.
    pub created_at: i64,
}

#[cfg(test)]
mod tests;
