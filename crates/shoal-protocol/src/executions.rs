//! Tracked executions, and what Shoal keeps of their agents to resume.
use crate::space_names::SpaceName;
use shoal_ids::execution_ids::ExecutionId;
use std::{fs, io, path::Path, time::Duration};

use serde::{Deserialize, Serialize};
use shoal_states::states;

use crate::{
    agent::ModelChoice,
    processes::Identity,
    sessions::Origin,
    state::{AgentState, TurnFailure},
};
use shoal_ids::agent_session_ids::AgentSessionId;

states!(
    /// What a tracked execution runs, which decides its lifecycle rules.
    ExecutionKind {
        /// A caller-chosen command in a ready workspace.
        Command => "command",
        /// A merge authorized by an unscoped caller; the daemon holds its Git gate.
        Merge => "merge",
        /// An unmerge authorized by an unscoped caller; the daemon holds its Git gate.
        Unmerge => "unmerge",
        /// The repository's setup command; exclusive apart from the execution that
        /// requested it, and its exit decides whether the workspace becomes ready.
        Setup => "setup",
    }
);

/// An agent a tracked wrapper runs, as its restores keep it.
#[derive(Debug, Clone)]
pub struct TrackedAgent {
    pub name: crate::agent::Agent,
    pub model: crate::agent::ModelChoice,
    pub mode: crate::config::modes::ModeName,
    /// The permissions `--permission` gives its agent, whatever its mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// Whether it begins a new session, rather than continuing or forking
    /// one, so its watches start from what its space follows now.
    pub new_session: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl TrackedAgent {
    /// Agent `name` running with the tool's own model and effort.
    pub fn named(name: impl Into<crate::agent::Agent>) -> Self {
        Self {
            narrowing: Default::default(),
            name: name.into(),
            model: Default::default(),
            mode: crate::config::modes::ModeName::DEFAULT,
            new_session: false,
        }
    }
}

/// The least time between dynamic starts, so their first reads do not collide.
pub const STAGGER: Duration = Duration::from_secs(10);

/// What tells a file at a path apart from its replacement. It follows
/// symlinks, so retargeting a link, renaming another file over it or rewriting
/// it in place all change the identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified: std::time::SystemTime,
}

impl FileIdentity {
    pub fn of(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = fs::metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified: metadata.modified()?,
        })
    }

    /// Whether another file now sits at `path`. A missing or unreadable file
    /// is no replacement.
    pub fn replaced_at(&self, path: &Path) -> bool {
        Self::of(path).is_ok_and(|current| current != *self)
    }
}

states!(
    /// How far a stop has progressed, so reconciliation after a daemon
    /// interruption knows which wrapper transition was still expected.
    Stage {
        /// The daemon decided to stop the agent and expects its wrapper to stop it.
        Stopping => "stopping",
        /// The wrapper stopped the agent, then pauses for an automatic restore
        /// or exits.
        Stopped => "stopped",
    }
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub agent: crate::agent::Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Unknown for records saved before stages existed, which may come from
    /// either side of the stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<Stage>,
    /// Where the agent ran, as its terminal host records it, so `shoal
    /// restore --all` can restore it there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// The wrapper that restores the agent automatically while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<Identity>,
    /// The session the agent's hooks last reported, which built-in agents
    /// resume by ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AgentSessionId>,
    /// The workspace moved where the agent's session is not available, such
    /// as from another machine, so the agent starts a new one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moved: bool,
    /// The workspace is a copy of another, so the agent starts from a fork
    /// of `session`, which stays the original's, or anew without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied: Option<Copied>,
    /// The workspace was a chat whose agent this one carries on, so it
    /// starts from a fork of `session` in the worktree, told what the space
    /// is now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub converted: bool,
    /// The agent takes the space's work over from another tool's, so it
    /// starts a new session told this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<HandoffBrief>,
    /// What the agent was doing when it stopped, as its hooks last reported
    /// it. Unknown for agents without hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<AgentState>,
    /// The model and effort the agent ran with, which its restore keeps.
    #[serde(default, flatten)]
    pub model: ModelChoice,
    /// The mode the agent started in, which its restore keeps.
    pub mode: crate::config::modes::ModeName,
    /// The `--permission` narrowing it started with, which its restore
    /// keeps.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// Load or free disk space stopped the agent, so the healthy readings of
    /// `[machine.overload.recovery]` decide its restore, whatever carries it
    /// out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub for_load: bool,
    /// How many times the kernel's OOM killer ended the agent since it last
    /// finished a turn, this stop included; zero when this stop was no OOM
    /// kill.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub oom_kills: u32,
    /// The earliest time its restore starts, after repeated OOM kills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restores_after: Option<crate::time::UnixTime>,
    /// What ended the agent, when its wrapper ended it unasked, so the stop
    /// is known even when the daemon never heard of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<crate::stop_causes::StopCause>,
}

impl Record {
    /// Whether the kernel's OOM killer ended the agent, as its wrapper or
    /// the daemon's count of kills says.
    pub fn oom_killed(&self) -> bool {
        self.oom_kills > 0 || self.cause == Some(crate::stop_causes::StopCause::OomKill)
    }
}

fn is_zero(count: &u32) -> bool {
    *count == 0
}

/// What the first session of an agent tool that takes a space's work over
/// is told: `told` before its mode's instructions, then `note`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffBrief {
    pub told: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How an agent of a copied workspace starts: from a fork of the session
/// it reported in the original, or anew.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Copied {
    /// The workspace's files come from a checkpoint older than the session,
    /// so the conversation describes work they do not contain.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub earlier_files: bool,
    /// It is no copy: a restored space's agent whose session another space
    /// holds, which forks it so both go on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held_elsewhere: bool,
}

/// Why and when Shoal stopped an agent, which its restore tells it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// When its recovery record was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<crate::time::UnixTime>,
    /// The kernel's OOM killer ended the agent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub oom_killed: bool,
}

/// An agent stopped in a removed workspace, which `shoal restore` saves
/// again for `shoal resume`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoppedAgent {
    pub execution: ExecutionId,
    pub record: Record,
    /// When its record was saved, which `shoal resume` shows as its stop
    /// time.
    pub stopped_at: crate::time::UnixTime,
}

/// An agent that exited on its own before its assignment was done, which
/// `shoal resume` continues. Unlike stopped work, it uses no heavy slot, never
/// delays cleanup and nothing restores it on its own. A workspace keeps one
/// per agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSession {
    /// The execution the agent ran as, which `--execution` selects. Its file
    /// is named after it.
    pub execution: ExecutionId,
    pub agent: crate::agent::Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AgentSessionId>,
    /// The workspace moved where the session is not available.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moved: bool,
    /// As [`Record::copied`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied: Option<Copied>,
    /// As [`Record::converted`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub converted: bool,
    /// As [`Record::handoff`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<HandoffBrief>,
    /// Where the agent ran, as its terminal host records it, so `shoal
    /// resume` can restore it there; unknown for one that exited on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Why and when Shoal stopped the agent; none for one that exited on its
    /// own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Stop>,
    /// What the agent was doing when it stopped or exited, as
    /// [`Record::turn`]; unknown, which counts as mid-turn, without hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<AgentState>,
    /// The model and effort the agent ran with, which its restore keeps.
    #[serde(default, flatten)]
    pub model: ModelChoice,
    /// The mode the agent started in, which its restore keeps.
    pub mode: crate::config::modes::ModeName,
    /// The `--permission` narrowing it started with, which its restore
    /// keeps.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
}

/// A space's retry of its agent's failed turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRetry {
    pub failure: TurnFailure,
    /// The attempt the retry makes or made, from 1.
    pub attempt: u32,
    /// When the agent is asked to continue.
    pub due_at: crate::time::UnixTime,
    /// Whether the agent was asked and its turn has not ended yet.
    pub delivered: bool,
}

/// A turn an API failure ended, and when its error says the upstream
/// refuses requests until, when it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailedTurn {
    pub failure: TurnFailure,
    pub resets_at: Option<crate::time::UnixTime>,
}

/// A failure whose error states no reset.
impl From<TurnFailure> for FailedTurn {
    fn from(failure: TurnFailure) -> Self {
        Self {
            failure,
            resets_at: None,
        }
    }
}

/// A retry the proxy waits to make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyRetry {
    pub failure: TurnFailure,
    /// The attempt that failed, from 1.
    pub attempt: u32,
    /// When the proxy tries again.
    pub next_at: crate::time::UnixTime,
}

states!(
    /// What an AI tool's upstream refuses requests for.
    ServiceLimit {
        /// A rate or usage limit.
        RateLimited => "rate_limited",
        /// The account's quota or credits are spent.
        Exhausted => "exhausted",
    }
);

/// A limit the proxy of an agent found its tool's upstream under, which
/// holds back that tool's queued agents while it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamLimit {
    pub limit: ServiceLimit,
    /// When the upstream said it lifts, such as a usage limit's reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<crate::time::UnixTime>,
}

/// What `shoal restore --discard-agent` discarded in a space.
#[derive(Debug, Serialize, Deserialize)]
pub struct DiscardedAgent {
    /// The name of the space that saved it.
    pub space: SpaceName,
    /// The execution IDs of the discarded stopped agents, last sessions and
    /// commands.
    pub executions: Vec<ExecutionId>,
    /// Its discarded launch that failed to start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<crate::model::QueueEntry>,
}

impl DiscardedAgent {
    /// Whether the space had nothing to discard.
    pub fn is_empty(&self) -> bool {
        self.executions.is_empty() && self.launch.is_none()
    }
}
