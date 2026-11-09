//! The agent launch queue and the work its entries wait for.
use crate::execution_ids::ExecutionId;
use crate::repository_ids::RepositoryId;
use crate::space_ids::SpaceId;
use crate::space_names::{SpaceName, SpaceSelector};
use crate::state::{QueueKind, QueueState};
use serde::{Deserialize, Serialize};

/// An agent launch or restore waiting for, or starting in, a `max_agents`
/// slot, or a CI job waiting for the runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub id: String,
    pub kind: QueueKind,
    pub repository_id: RepositoryId,
    /// The workspace name: derived from the planned branch for a launch, the
    /// stopped agent's for a restore.
    pub name: String,
    pub agent: String,
    /// The mode a launch's agent starts in; none for a restore, which keeps
    /// its agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The `--permission` narrowing a launch's agent started with, which narrows the
    /// permissions of its mode.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
    pub issue_title: Option<String>,
    pub state: QueueState,
    /// The 1-based place among waiting entries; `None` once starting.
    pub position: Option<usize>,
    pub created_at: crate::time::UnixTime,
    /// The workspace a starting launch opened, or a restore's own.
    pub workspace_id: Option<SpaceId>,
    /// The stopped execution a restore restores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionId>,
    /// The existing workspace a launch in it opens, known when it is queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opens: Option<SpaceId>,
    /// The work a waiting launch still waits for: workspace and queued launch
    /// names and issue URLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_for: Vec<String>,
    /// Whether a restore waits for an update for its agent, which stopped
    /// between turns, rather than restoring an interrupted one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waits_for_update: bool,
    /// The branch of the completed dependency a started launch builds on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The CI job a job's entry claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<Box<QueuedJob>>,
}

/// A CI job of a repository's runner, from the forge's queue, with the run
/// it belongs to and the workspace that run's PR or branch links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedJob {
    pub job: crate::forge::ci::runner::WaitingJob,
    pub run: crate::forge::ci::runner::JobRun,
    /// The workspace that links the run's PR, or owns the branch it ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<SpaceId>,
}

impl QueueEntry {
    /// The workspace the entry starts its agent in, once known.
    pub fn workspace(&self) -> Option<&SpaceId> {
        self.opens.as_ref().or(self.workspace_id.as_ref())
    }
}

/// What stopping a workspace did: the agent launches waiting to start in it
/// that it removed from the queue, or for a CI job's workspace the run it
/// asked the forge to cancel, whose runner then stops the job.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoppedWorkspace {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled: Vec<QueueEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_run: Option<String>,
}

/// An entry `shoal queue move` placed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovedEntry {
    pub entry: QueueEntry,
    /// The entry is the restore of a running agent the move stopped.
    pub stopped: bool,
}

/// Where `shoal queue move` places an entry among the waiting ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuePlace {
    /// The 1-based position, as `shoal queue` numbers them.
    Position(usize),
    /// In front of the entry with this ID.
    Before(String),
    Last,
}

/// Work a queued launch waits for until it is complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dependency {
    /// A workspace or a queued launch of the same repository, by name or ID.
    Work(String),
    /// An issue, by URL.
    Issue(String),
}

/// An agent launch `shoal start` asks the daemon to admit or queue. The plan
/// is JSON in the format of the binary's `start_plan::AddPlan`, which the
/// daemon also writes for the launches it queues itself and the
/// `queue-start` worker reads back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueRequest {
    pub repository: crate::repository::Selector,
    pub name: SpaceName,
    pub agent: String,
    /// The mode the agent starts in; the default mode when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The `--permission` narrowing the agent started with, which narrows the
    /// permissions of its mode.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
    pub issue_title: Option<String>,
    pub plan: String,
    /// Terminal multiplexer variables the start needs to reach the same window,
    /// and the caller's output setting.
    pub env: std::collections::BTreeMap<String, String>,
    /// Start at once regardless of the limits; the agent still takes a slot.
    /// Scoped callers cannot.
    pub now: bool,
    /// The existing workspace the launch opens, such as a child workspace for
    /// its parent's agent. The daemon then starts the entry itself, outside
    /// the caller's processes, whether or not a limit applies.
    #[serde(default)]
    pub workspace: Option<SpaceSelector>,
    /// The work the launch waits for; it always queues while any remains.
    #[serde(default)]
    pub after: Vec<Dependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueAdmission {
    /// Whether the caller starts the agent now.
    pub start: bool,
    /// `None` when no limit applies, so nothing tracks the launch.
    pub entry: Option<QueueEntry>,
    /// Entries waiting, including this one when it waits.
    pub waiting: usize,
}

/// What a queued start runs: the plan the entry was queued with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedPlan {
    pub entry: QueueEntry,
    pub plan: String,
}
