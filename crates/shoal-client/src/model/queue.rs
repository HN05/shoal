//! The agent launch queue and what blocks its entries.
use crate::execution_ids::ExecutionId;
use crate::repository_ids::RepositoryId;
use crate::space_ids::SpaceId;
use crate::space_names::{SpaceName, SpaceSelector};
use crate::state::{QueueKind, QueueState};
use serde::{Deserialize, Serialize};

use crate::forge::items::{
    link::LinkKind,
    reference::{ItemInput, ItemRef},
};

/// An agent launch or restore waiting for, or starting in, a `max_agents`
/// slot, or a CI job waiting for the runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub id: crate::queue::QueueEntryId,
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
    /// What kept a waiting agent or job from starting at the last admission
    /// pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<HeldBy>,
    /// Where a waiting entry ranks, which decides what starts first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<QueueRank>,
}

/// A waiting entry's rank: its priority and tier, each with the work it
/// took that from when that work waits on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueRank {
    pub priority: crate::priority::Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority_from: Option<InheritedRank>,
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_from: Option<InheritedRank>,
    /// The areas a launch of a new workspace shares with the workspaces of
    /// its repository, which put it after launches of its tier that share
    /// fewer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_areas: Vec<SharedArea>,
}

/// An area a launch shares, with the spaces holding it: their numbers, or
/// their names without one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedArea {
    pub area: String,
    pub spaces: Vec<String>,
}

/// How close an entry's work is to finished; later tiers start first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Launches, and CI runs started without a PR.
    New,
    /// Restores of agents that have worked in their space.
    Started,
    /// Work of a space with an open PR or a ready mark, and CI jobs on open
    /// PRs and on the default branch.
    Finishing,
}

/// The work an entry took its rank from: `from` is its space's number, or a
/// queued launch's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InheritedRank {
    pub from: String,
    pub dependency: Dependency,
}

/// Why work waits on an entry's, so that the entry takes its rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dependency {
    /// It is blocked on the entry's work: an `--after` blocker, one its
    /// issue's forge records, or a progress blocker.
    Blocked,
    /// It is the parent workspace of the entry's space.
    Parent,
    /// It is a child of the entry's space and asks it a question.
    Question,
    /// It is a workspace stacked on the entry's.
    Stacked,
    /// It waits for a lease the entry's space holds.
    Lease,
    /// The entry is a CI job or review for it.
    Serves,
}

/// What keeps a waiting entry from starting, besides the work it waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum HeldBy {
    /// Memory below the warning threshold has `left` bytes, no more than the
    /// `needed` bytes the entry is expected to use.
    Memory { needed: u64, left: u64 },
    /// Memory is kept for a higher-ranked entry that has waited for it as
    /// long as work may start ahead of it.
    MemoryKept,
    /// Free disk space is below the warning threshold.
    Disk,
    /// `machine.max_agents` is reached.
    MaxAgents,
    /// `machine.max_jobs` is reached.
    MaxJobs,
    /// The repository's `agent.max_running` is reached.
    RepositoryLimit,
    /// The repository's `runner.permit` pool has no free permit.
    Permits,
    /// A memory or disk reading is critical or missing.
    Reading,
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
    /// The spaces its run is for: the workspace that links the run's PR or
    /// owns the branch it ran on, first, then the chats that link its PR.
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

/// What stopping a workspace did: the agent launches waiting to start in it
/// that it removed from the queue, how many untracked processes it stopped
/// and whether it saved work for `shoal restore`, or for a CI job's workspace
/// the run it asked the forge to cancel, whose runner then stops the job.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoppedSpace {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled: Vec<QueueEntry>,
    pub other_processes: usize,
    pub saved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_run: Option<String>,
}

/// What blocks work until it clears. A URL of another kind than the
/// variant's is refused as the blocker is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", try_from = "BlockerFields")]
pub enum Blocker {
    /// A space, or for a launch a queued launch of the same repository, by
    /// name or ID.
    Space(String),
    /// An issue, by URL, or by number in the space's repository.
    Issue(ItemInput),
    /// A PR, by URL.
    Pr(ItemRef),
    /// A CI run, by URL.
    CiRun(ItemRef),
}

/// How a [`Blocker`] reads, before its URL's kind is checked.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum BlockerFields {
    Space(String),
    Issue(ItemInput),
    Pr(ItemRef),
    CiRun(ItemRef),
}

impl TryFrom<BlockerFields> for Blocker {
    type Error = anyhow::Error;

    fn try_from(fields: BlockerFields) -> anyhow::Result<Self> {
        let (blocker, url) = match fields {
            BlockerFields::Space(space) => return Ok(Self::Space(space)),
            BlockerFields::Issue(ItemInput::Url(url)) => (
                Self::Issue(ItemInput::Url(url.clone())),
                Some((url, LinkKind::Issue)),
            ),
            BlockerFields::Issue(number) => (Self::Issue(number), None),
            BlockerFields::Pr(url) => (Self::Pr(url.clone()), Some((url, LinkKind::Pr))),
            BlockerFields::CiRun(url) => (Self::CiRun(url.clone()), Some((url, LinkKind::Ci))),
        };
        if let Some((url, kind)) = url {
            url.of_kind(kind)?;
        }
        Ok(blocker)
    }
}

/// An agent launch `shoal start` asks the daemon to admit or queue. The plan
/// is JSON in the format of the binary's `start_plan::AddPlan`, which the
/// daemon also writes for the launches it queues itself and the
/// `queue-start` worker reads back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueRequest {
    pub repository: crate::repository::Selector,
    pub name: SpaceName,
    pub agent: crate::agent::Agent,
    /// The mode the agent starts in; the default mode when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::config::modes::ModeName>,
    /// The permissions `--permission` gave the agent, which apply as
    /// given whatever its mode's.
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
    /// A scoped caller does as its `limits` permission allows.
    pub now: bool,
    /// The existing workspace the launch opens, such as a child workspace for
    /// its parent's agent. The daemon then starts the entry itself, outside
    /// the caller's processes, whether or not a limit applies, unless
    /// `starts_in_caller`.
    #[serde(default)]
    pub workspace: Option<SpaceSelector>,
    /// The caller starts the launch in `workspace` itself once admitted, as
    /// it starts a new workspace's, rather than the daemon.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starts_in_caller: bool,
    /// What blocks the launch; it always queues while any remains.
    #[serde(default)]
    pub blockers: Vec<Blocker>,
    /// The priority of the space the launch creates, which ranks it until
    /// that space exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<crate::priority::Priority>,
    /// The labels of the issue the launch works on, whose
    /// `[priority.labels]` priority ranks it when it is given none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issue_labels: Vec<String>,
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

#[cfg(test)]
mod tests;
