//! The agent launch queue.
use crate::space_names::{SpaceName, SpaceSelector};
use crate::state::{QueueKind, QueueState};
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_schedule::rank::QueueRank;

use crate::progress::Blocker;

/// An agent launch or restore waiting for, or starting in, a `max_heavy_agents`
/// slot, or a CI job waiting for the runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub id: shoal_ids::queue_entry_ids::QueueEntryId,
    pub kind: QueueKind,
    /// The repository it starts in; none for a restore in a chat without
    /// one.
    pub repository_id: Option<RepositoryId>,
    /// The space's name: derived from the planned branch for a launch, the
    /// stopped agent's space's for a restore.
    pub name: SpaceName,
    pub agent: crate::agent::Agent,
    /// The mode a launch's agent starts in; none for a restore, which keeps
    /// its agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::config::modes::ModeName>,
    /// The permissions `--permission` gave a launch's agent, which apply as
    /// given whatever its mode's.
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
    /// The space a starting launch opened, or a restore's own.
    pub space_id: Option<SpaceId>,
    /// The stopped execution a restore restores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionId>,
    /// The existing workspace a launch in it opens, known when it is queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opens: Option<SpaceId>,
    /// The space whose agent queued a launch of a new workspace, which the
    /// launch opens in that space's scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_by: Option<SpaceId>,
    /// What still blocks a waiting launch: workspace and queued launch
    /// names and issue URLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
    /// Whether a restore waits for an update for its agent, which stopped
    /// between turns, rather than restoring an interrupted one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waits_for_update: bool,
    /// The branch of the done blocker a started launch builds on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The CI job a job's entry claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<Box<QueuedJob>>,
    /// The command a waiting `shoal run` runs, by its configured name or
    /// its arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// What kept a waiting agent or job from starting at the last admission
    /// pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<HeldBy>,
    /// Where a waiting entry ranks, which decides what starts first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<QueueRank>,
    /// Why a failed launch did not start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What keeps a waiting entry from starting, besides the work it waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum HeldBy {
    /// Memory below the warning threshold has `left` bytes, no more than the
    /// `needed` bytes the entry is expected to use.
    Memory { needed: u64, left: u64 },
    /// Memory is kept for `behind`, the name of a higher-ranked entry that
    /// waits for it: of another priority or tier, or of the same one once it
    /// has waited as long as work may start ahead of it.
    MemoryKept { behind: String },
    /// Free disk space is below the warning threshold.
    Disk,
    /// `machine.max_heavy_agents` is reached.
    MaxHeavyAgents,
    /// `machine.max_jobs` is reached.
    MaxJobs,
    /// The repository's `agent.max_heavy_running` is reached.
    RepositoryLimit,
    /// The repository's `runner.permit` pool has no free permit.
    Permits,
    /// `ahead` machines of the hub rank ahead of this one for the CI job,
    /// which may claim it first.
    RankedBehind { ahead: usize },
    /// A memory or disk reading is critical or missing, or, for a job beside
    /// running ones, the CPU reading.
    Reading,
    /// The machine's CPU, niced time included, was `busy` percent busy, at
    /// or above the `level` percent a job beside running ones waits at.
    Cpu { busy: u8, level: u8 },
    /// The proxy of a running agent of the same tool found its upstream
    /// under `limit`, which stands until `until`.
    UpstreamLimit {
        limit: crate::executions::ServiceLimit,
        until: crate::time::UnixTime,
    },
}

/// A CI job of a repository's runner, from the forge's queue, with the run
/// it belongs to and the spaces that run's PR or branch links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedJob {
    pub job: crate::forge::ci::runner::WaitingJob,
    pub run: crate::forge::ci::runner::JobRun,
    /// The spaces its run is for: those that link the run's PR, workspaces
    /// first, or the workspace that owns the branch it ran on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub space_ids: Vec<SpaceId>,
    /// Whether its run is on the repository's default branch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default_branch: bool,
}

impl QueueEntry {
    /// The repository a CI job's entry runs in, which every job's names.
    pub fn job_repository(&self) -> anyhow::Result<&RepositoryId> {
        use anyhow::Context as _;
        (self.repository_id.as_ref()).context("the job's entry names no repository")
    }

    /// The space the entry starts its agent in, once known.
    pub fn space(&self) -> Option<&SpaceId> {
        self.opens.as_ref().or(self.space_id.as_ref())
    }
}

/// What stopping a workspace did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoppedSpace {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled: Vec<QueueEntry>,
    pub other_processes: usize,
    pub saved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_run: Option<String>,
}

/// An agent launch in a recorded space to admit or queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueRequest {
    pub name: SpaceName,
    pub start: QueuedStart,
    /// Terminal multiplexer variables the start needs to reach the same window,
    /// and the caller's output setting.
    pub env: std::collections::BTreeMap<String, String>,
    /// The recorded space the launch starts its agent in, in that space's
    /// repository. The daemon starts the entry itself, outside the caller's
    /// processes, whether or not a limit applies, unless the launch runs
    /// `here`.
    pub space: SpaceSelector,
    /// What blocks the launch; it always queues while any remains.
    #[serde(default)]
    pub blockers: Vec<Blocker>,
}

/// The plan of a queued agent start, the same for every kind of space: the
/// launch of the agent of the recorded space its entry names, and the PR it
/// links as it starts, which the `queue-start` worker carries out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedStart {
    pub launch: crate::protocol::operations::QueuedLaunch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<crate::forge::items::reference::ItemInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueAdmission {
    /// Whether the caller starts the agent now.
    pub start: bool,
    /// The launch's entry; `None` once an agent took it over.
    pub entry: Option<QueueEntry>,
    /// Entries waiting, including this one when it waits.
    pub waiting: usize,
}

/// What a queued start runs: the plan the entry was queued with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedPlan {
    pub entry: QueueEntry,
    pub start: QueuedStart,
}
