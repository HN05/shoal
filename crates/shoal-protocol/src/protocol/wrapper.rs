//! The execution wrapper's messages: reattachment, execution events and
//! the daemon's controls.
use crate::model::Protection;
use crate::processes::Identity;
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;

/// The first protocol whose wrappers read [`Control::RestoreWaits`].
pub const RESTORE_WAITS: u32 = 174;

/// The first protocol whose wrappers restore their agent under the
/// narrowing a [`Control::Resume`] gives.
pub const RESUME_NARROWING: u32 = 241;

/// A wrapper's request to restore its execution connection after the daemon
/// went away. It is permanent: daemons accept it at every protocol version, so
/// wrappers from older releases can reattach after an upgrade. Add fields only
/// with defaults, and keep the execution events and controls such a wrapper
/// understands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reattach {
    pub execution: ExecutionId,
    pub wrapper: Identity,
    pub child: Option<Identity>,
    pub group_id: u32,
    pub scope_token: crate::scope_tokens::ScopeToken,
    /// Transient agent metadata the daemon lost with its memory.
    #[serde(default)]
    pub agent: Option<crate::agent::Agent>,
    /// The model and effort the agent runs with.
    #[serde(default, skip_serializing_if = "crate::agent::ModelChoice::is_empty")]
    pub model: crate::agent::ModelChoice,
    /// The mode the agent started in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::config::modes::ModeName>,
    /// The permissions `--permission` gives the agent, whatever the mode's.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
    #[serde(default)]
    pub recover: bool,
    /// How long the command has run, keeping the overload stop order.
    #[serde(default)]
    pub running_ms: u64,
    /// Set while the wrapper waits to restore an agent that protection
    /// stopped; `child` and `group_id` then name the stopped child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_restore: Option<Protection>,
    /// Set from a demo's report that its child exited until the next run
    /// registers; `child` and `group_id` name the exited child, which the
    /// daemon may already have forgotten.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub between_runs: bool,
}

/// Wrapper → daemon messages after an [`ExecutionMethod::Execute`] response.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionEvent {
    Started {
        child: Option<Identity>,
        group_id: u32,
        /// The child restores a saved agent, which leaves the workspace's
        /// other last sessions; a new agent replaces them.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        restored: bool,
        /// The saved records the child restores from, which the wrapper
        /// consumes once the start is acknowledged.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        restored_from: Vec<std::path::PathBuf>,
    },
    Finished {
        exit_code: i32,
        /// What the scope the command ran in recorded of its memory, where
        /// it ran in one of its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        memory: Option<ScopeMemory>,
    },
    Paused,
    /// The user woke the agent whose restore the wrapper waits for, in the
    /// place it waits in: start that restore now, whatever the limits, as
    /// `shoal queue start` does. Ignored while no restore of it waits.
    Wake,
    /// The child exited and the wrapper starts another, as a demo's does,
    /// once the daemon answers [`Control::ChildReleased`].
    ChildExited,
    /// The wrapper continues from the installed binary, which an upgrade put
    /// in place of its own, between a [`Control::Resume`] that allows it and
    /// its next child's start, and registers again from `origin`, the place it runs in. The
    /// daemon answers [`Control::Registered`], or a stop.
    Register {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Box<crate::sessions::Origin>>,
    },
}

/// What the scope a tracked command ran in recorded of its memory, over
/// every run of it, as a demo's restarts make.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeMemory {
    /// How many of its processes the kernel's OOM killer killed.
    pub oom_kills: u64,
    /// The most memory it held that reclaim could not free, with what it
    /// swapped out, in bytes, as the wrapper read it.
    pub peak: u64,
}

impl ScopeMemory {
    /// What `self` and `other`, the records of two runs, recorded together.
    pub fn and(self, other: Self) -> Self {
        Self {
            oom_kills: self.oom_kills.saturating_add(other.oom_kills),
            peak: self.peak.max(other.peak),
        }
    }
}

/// Daemon → wrapper messages during an execution.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Started,
    /// Stop without saving anything to resume. Older wrappers ignore the
    /// reason and `removed`.
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The workspace is being removed; its removal record keeps the agent
        /// whose restore this cancels.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        removed: bool,
    },
    /// Stop and save what `shoal resume` restores. Older wrappers ignore the
    /// reason.
    Pause {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The workspace is being removed; its removal record keeps the session.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        removed: bool,
        /// The system is shutting down, which may end the wrapper before its
        /// command: save the session first, as a signal ending it does.
        /// Older wrappers pause as `shoal stop` does.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        shutdown: bool,
    },
    OverloadStop {
        recover: bool,
        #[serde(default = "default_overload_reason")]
        reason: String,
        /// The condition automatic recovery waits for, completing "once …".
        #[serde(default = "default_resumes_when")]
        resumes_when: String,
    },
    Resume {
        ports: Vec<crate::model::PortReservation>,
        /// The binary the daemon runs, which a wrapper whose own binary an
        /// upgrade replaced may continue from, since it speaks the daemon's
        /// protocol. Older daemons leave it out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        executable: Option<crate::executions::FileIdentity>,
        /// The `--permission` narrowing the agent restores under in place of
        /// the one it ran with, which only wrappers of [`RESUME_NARROWING`]
        /// and later read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        narrowing: Option<crate::config::permissions::PermissionsConfig>,
    },
    Finished {
        complete: bool,
    },
    /// What the exited child left is stopped and the worktree is still the
    /// recorded one, so the wrapper may start its next run.
    ChildReleased,
    /// The daemon is shutting down; keep the command running and reattach.
    Detach,
    /// The plan of an execution whose wrapper registered again.
    Registered {
        plan: Box<crate::model::ExecutionPlan>,
    },
    /// The waiting restore also waits until the stopped agent's remaining
    /// processes exit, which `issue` names. Only wrappers of
    /// [`RESTORE_WAITS`] and later read it.
    RestoreWaits {
        issue: String,
    },
}

fn default_overload_reason() -> String {
    "system overload".into()
}

/// What memory and CPU protection wait for, and what older daemons meant.
pub fn default_resumes_when() -> String {
    "system load is healthy".into()
}
