//! A space's status and the workspace inspection it builds on.
use super::{
    ChangedOnBothSides, Chat, CheckoutSync, CleanupReason, Completion, Execution, PortReservation,
    ReviewMark, SpaceLinks, SpaceRef, Workspace,
};
use crate::resources::ResourceLease;
use crate::sim::Simulator;
use crate::space_ids::SpaceId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Inspection {
    pub issue: Option<crate::links::issue::Registration>,
    pub completion: Option<Completion>,
    pub pr_cleanup: Option<crate::links::pr::Registration>,
    pub workspace: Workspace,
    pub executions: Vec<Execution>,
    /// The places Shoal created for the workspace, such as a session whose
    /// launch failed and left its shell.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<crate::sessions::Origin>,
    pub ports: Vec<PortReservation>,
    pub resources: Vec<ResourceLease>,
    pub simulators: Vec<Simulator>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
}

/// A space's status: what every space has, and under `kind` the space's
/// own record with what only its kind has.
#[derive(Debug, Serialize, Deserialize)]
pub struct SpaceStatus {
    #[serde(flatten)]
    pub space: SpaceDetail,
    pub issue: Option<crate::links::issue::Registration>,
    pub completion: Option<Completion>,
    pub pr_cleanup: Option<crate::links::pr::Registration>,
    #[serde(flatten)]
    pub attention: Attention,
    /// The current state of each watched PR.
    #[serde(default)]
    pub prs: Vec<crate::forge::review::status::PrStatus>,
    /// The linked issue's current state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_status: Option<IssueStatus>,
    /// What its agents may do, each with where it comes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<crate::config::permissions::Permissions>,
    /// Why its permissions could not be resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions_error: Option<String>,
}

/// What awaits the user about a space.
#[derive(Debug, Serialize, Deserialize)]
pub struct Attention {
    /// Unread notifications about the space.
    pub unread_notifications: u64,
    /// Questions its agents asked that wait for an answer.
    #[serde(default)]
    pub open_questions: usize,
    /// Approval requests that wait for a decision: those a workspace made,
    /// or those a chat decides.
    #[serde(default)]
    pub pending_approvals: usize,
    /// Files attached to it, including those its completion handed back.
    #[serde(default)]
    pub attached_files: usize,
}

/// The space a status describes: a workspace with what its worktree adds,
/// or a chat.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpaceDetail {
    Workspace(Box<WorktreeStatus>),
    Chat { chat: Box<Chat> },
}

impl SpaceDetail {
    pub fn id(&self) -> &SpaceId {
        match self {
            Self::Workspace(status) => &status.workspace.id,
            Self::Chat { chat } => &chat.id,
        }
    }

    /// What user-facing text calls it: `workspace` or `chat`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Workspace(_) => "workspace",
            Self::Chat { .. } => "chat",
        }
    }

    /// The workspace and what its worktree adds; a chat has none.
    pub fn worktree(&self) -> Option<&WorktreeStatus> {
        match self {
            Self::Workspace(status) => Some(status),
            Self::Chat { .. } => None,
        }
    }

    /// The issues, PRs and CI runs it links.
    pub fn links(&self) -> &SpaceLinks {
        match self {
            Self::Workspace(status) => &status.workspace.links,
            Self::Chat { chat } => &chat.links,
        }
    }

    /// Where its agent is in its work.
    pub fn progress(&self) -> Option<&crate::progress::Progress> {
        match self {
            Self::Workspace(status) => status.workspace.progress.as_ref(),
            Self::Chat { chat } => chat.progress.as_ref(),
        }
    }

    /// Its ready-for-review marks.
    pub fn review(&self) -> &[ReviewMark] {
        match self {
            Self::Workspace(status) => &status.workspace.review,
            Self::Chat { chat } => &chat.review,
        }
    }
}

/// A workspace's status that needs its worktree, with its record.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorktreeStatus {
    pub workspace: Workspace,
    pub executions: Vec<Execution>,
    /// The places Shoal created for the workspace, such as a session whose
    /// launch failed and left its shell.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<crate::sessions::Origin>,
    pub ports: Vec<PortReservation>,
    pub resources: Vec<ResourceLease>,
    pub simulators: Vec<Simulator>,
    pub setup_finished: bool,
    pub diff: Option<DiffSummary>,
    pub diff_error: Option<String>,
    /// Why the daemon's automatic cleanup fails in this workspace.
    #[serde(default)]
    pub cleanup_error: Option<String>,
    /// Why automatic cleanup keeps this workspace now.
    pub cleanup_reason: CleanupReason,
    /// When automatic cleanup removes the workspace, while its grace period
    /// runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_at: Option<crate::time::UnixTime>,
    /// The retry of the agent's turn an API failure ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_retry: Option<crate::executions::TurnRetry>,
    /// The request the agent's proxy waits to retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_retry: Option<crate::executions::ProxyRetry>,
    /// How many processes are still running after what started them exited,
    /// as the cleanup sweep last found them.
    #[serde(default)]
    pub leftover_count: usize,
    /// The first of them.
    #[serde(default)]
    pub leftovers: Vec<crate::processes::Leftover>,
    /// How the background sync of the repository's default branch went,
    /// once the daemon has tried it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<CheckoutSync>,
    /// The last check against the default conflict target, when it checked
    /// the committed HEAD; boxed to keep the response small.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_on_both_sides: Option<Box<ChangedOnBothSides>>,
    /// The workspaces of the CI jobs running for its PR or branch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<Workspace>,
    /// The chats running or stopped in its worktree, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<SpaceRef>,
}

/// A failed lookup leaves `state` empty and records its error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueStatus {
    /// `open` or `closed`.
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
