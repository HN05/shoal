use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::execution_ids::ExecutionId;

use super::{Forwarding, Lifetime, Target};

/// Requests between a workspace's agents and the daemon: their state, messages and updates.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMethod {
    /// The caller's execution, when scoped, is recorded as the reporter,
    /// and `session` as the agent session it resumes. `turn_end` comes with
    /// an idle report that ends a turn.
    SetAgentState {
        workspace: SpaceSelector,
        state: crate::state::AgentState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<AgentSessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_end: Option<TurnEnd>,
        /// When the hook made the report, which orders it among those that
        /// arrive late; when it arrives, without one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        made: Option<std::time::SystemTime>,
    },
    /// Change the space's progress, as its agent or its plan tool's hook
    /// reports it; answers with the progress after the change.
    SetProgress {
        space: SpaceSelector,
        change: crate::progress::ProgressChange,
    },
    /// The space's progress.
    Progress { space: SpaceSelector },
    /// Let the space's agent sleep as `sleep` says, so Shoal does not wake
    /// it for having stayed idle, replacing its timer with `timer` when one
    /// is given, or end its sleep and cancel its timer with none; a space
    /// whose mode never wakes it records none.
    Sleep {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sleep: Option<crate::state::Sleep>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timer: Option<crate::sleep::WakeTimer>,
    },
    /// [`Self::SetAgentState`] working for the turn a wake hook starts with
    /// an update, unless a stop of the caller's agent came first, which
    /// leaves the update for its restore. Answers whether the turn starts.
    StartWokenTurn { workspace: SpaceSelector },
    /// [`Self::SetAgentState`] idle for a turn `failure` ended, which may
    /// schedule a retry of the turn, after `resets_at` when the turn's
    /// error says its upstream refuses requests until then. An unscoped
    /// caller, such as the wrapper whose proxy ended the turn, names the
    /// reporting `execution`; a scoped one reports for its own.
    ReportTurnFailure {
        workspace: SpaceSelector,
        failure: crate::state::TurnFailure,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resets_at: Option<crate::time::UnixTime>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<AgentSessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution: Option<ExecutionId>,
        #[serde(default, skip_serializing_if = "TurnEnd::is_empty")]
        turn_end: TurnEnd,
        /// When the hook made the report, as for [`Self::SetAgentState`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        made: Option<std::time::SystemTime>,
    },
    /// A subagent of the caller's agent started or stopped, or its session
    /// started, as its hooks report it, which the daemon counts among the
    /// caller's execution's running subagents.
    ReportSubagent {
        space: SpaceSelector,
        change: SubagentChange,
    },
    /// A request of a subagent of `execution`'s agent passed its proxy,
    /// which its wrapper reports, so a subagent whose stop was lost stops
    /// counting once its requests stop.
    ReportSubagentRequest {
        space: SpaceSelector,
        execution: ExecutionId,
    },
    /// The wake hook of the space's agent ended for `ended` without
    /// starting the agent's turn, which the daemon logs so that a missed
    /// wake can be traced.
    ReportWakeEnd {
        space: SpaceSelector,
        ended: WakeEnd,
    },
    /// The retry the proxy of `execution` waits to make, which its wrapper
    /// reports; `None` once the proxy succeeded or gave up.
    ReportProxyRetry {
        workspace: SpaceSelector,
        execution: ExecutionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry: Option<crate::executions::ProxyRetry>,
    },
    /// The upstream of `execution`'s proxy refuses requests until Unix
    /// second `resets_at`, such as a usage limit's reset, which its wrapper
    /// reports so a retry of the agent's turn waits past it.
    ReportUpstreamReset {
        workspace: SpaceSelector,
        execution: ExecutionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resets_at: Option<crate::time::UnixTime>,
    },
    /// The limit the upstream of `execution`'s proxy is under, which its
    /// wrapper reports so queued agents of the same tool wait it out;
    /// `None` once another proxy replaces it or none runs.
    ReportUpstreamLimit {
        workspace: SpaceSelector,
        execution: ExecutionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<crate::executions::UpstreamLimit>,
    },
    /// The upstream answered a request of `execution`'s proxy, which its
    /// wrapper reports so the limits other agents of its tool met lift.
    ReportUpstreamAnswered {
        workspace: SpaceSelector,
        execution: ExecutionId,
    },
    /// The agent `execution` runs read a key from its terminal at `at`, as
    /// its wrapper finds by the terminal's access time.
    ReportInput {
        workspace: SpaceSelector,
        execution: ExecutionId,
        at: crate::time::UnixTime,
    },
    /// Shoal types into the terminal of the agent `execution` runs now, so
    /// the keys it reads next are not the user's.
    ReportTyping {
        workspace: SpaceSelector,
        execution: ExecutionId,
    },
    /// The caller attached to the sessions of the space.
    ReportAttach { space: SpaceSelector },
    /// Long-lived: answers once whether the workspace's agent is idle with
    /// undelivered messages or queued item updates matches `due`; with
    /// `hooked`, the execution whose agent a hook of its own wakes, only
    /// while that hook cannot: after an interrupt ended its last turn, or
    /// once no wait for the workspace's updates has run for a few minutes.
    AwaitAgentWake {
        workspace: SpaceSelector,
        due: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hooked: Option<ExecutionId>,
    },
    /// Connected agents, and executions whose wrapper is gone, which may
    /// run one, of the spaces the caller reaches.
    ListAgents,
    /// The agents connected in the space, a workspace or a running chat,
    /// with their turns and places.
    SpaceAgents { space: SpaceSelector },
    /// An interrupt the caller typed ended the turn of the agent `execution`
    /// runs in the space, as [`Self::SpaceAgents`] lists it, which leaves it
    /// idle.
    InterruptTurn {
        space: SpaceSelector,
        execution: ExecutionId,
    },
    /// Restart the agent of `space` as `restart` says, answering once the
    /// daemon restarted it, recorded the restart for later or skipped it.
    RestartAgent {
        space: SpaceSelector,
        restart: crate::model::AgentRestart,
    },
    /// Drop the restart the daemon recorded for the agent of `space`,
    /// answering with the one it dropped, if any.
    CancelRestart { space: SpaceSelector },
    /// Resume the agent of `space` as `resume` says, answering once the
    /// daemon continued its turn, queued or started its restores, or found
    /// nothing to resume.
    ResumeAgent {
        space: SpaceSelector,
        resume: crate::model::AgentResume,
    },
    /// The session of the space's agent that a fork starts from: the
    /// caller's own agent there, else its only one.
    AgentSession { space: SpaceSelector },
    /// The unread alerts and queued item updates of a
    /// workspace or running chat, without waiting or polling the forge;
    /// acknowledge them with [`Self::AcknowledgePrUpdates`]. It replaces a
    /// running watch.
    AgentUpdates { space: SpaceSelector },
    /// Long-lived: what [`Self::AgentUpdates`] answers, once there is any.
    WaitAgentUpdates { space: SpaceSelector },
    /// Whether messages that wake an idle agent or item updates a hook
    /// would deliver wait for the agent of the space, without taking them.
    UpdatesWaiting { space: SpaceSelector },
    /// The caller's login for Git over HTTPS to `host`: the agent token
    /// `[machine.forges]` sets for it, for any scoped caller; none for an
    /// unscoped caller or a host without one.
    GitCredential { host: String },
    AcknowledgePrUpdates {
        space: SpaceSelector,
        deliveries: Vec<String>,
        /// Alerts the watch printed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        messages: Vec<i64>,
        /// The PIDs of the leftovers the hook printed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        leftovers: Vec<u32>,
    },
}

/// Whether updates wait for a space's agent, as
/// [`AgentMethod::UpdatesWaiting`] answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingUpdates {
    pub waiting: bool,
}

/// Whether the turn a wake hook starts goes ahead: not when a stop of the
/// agent came first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WokenTurn {
    pub starts: bool,
}

/// Why a wake hook ended without starting its agent's turn, apart from a
/// newer delivery taking its updates, which the daemon sees itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeEnd {
    /// The agent's process exited.
    AgentExited,
    /// The agent's process could not be inspected, which counts as exited.
    AgentUnknown,
    /// A stop of the agent came before the turn the hook would start.
    Stopped,
    /// `signal` ended the hook.
    Signalled { signal: String },
}

/// What an agent reports when a turn ends.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEnd {
    /// How long its prompt cache lives after the turn, as its own settings
    /// give it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_secs: Option<u64>,
    /// The turn's final message, which is the question when the agent
    /// marked the turn as asking the user in its chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_message: Option<String>,
}

impl TurnEnd {
    /// Whether it reports nothing beyond the turn's end.
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A change to the subagents an agent runs, as its hooks report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentChange {
    /// The subagent its AI tool names `agent_id` started.
    Started { agent_id: String },
    /// The subagent its AI tool names `agent_id` stopped.
    Stopped { agent_id: String },
    /// Its session started, resumed or cleared, which leaves none of the
    /// subagents from before running.
    SessionStarted,
}

impl AgentMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::AwaitAgentWake { .. } | Self::WaitAgentUpdates { .. } => Lifetime::Resumed,
            // A restart waits for requests of the wrapper it restores.
            Self::RestartAgent { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}

impl AgentMethod {
    /// Where the request runs when its space is another machine's: its
    /// agent's hooks and wrapper report from the machine the agent runs on,
    /// and the rest read or change Shoal's state.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::SetProgress { .. }
            | Self::Progress { .. }
            | Self::Sleep { .. }
            | Self::SpaceAgents { .. }
            | Self::InterruptTurn { .. }
            | Self::RestartAgent { .. }
            | Self::CancelRestart { .. }
            | Self::ResumeAgent { .. } => Forwarding::Online,
            // A fork starts from the session files where the caller runs;
            // the list is of this machine's agents.
            Self::AgentSession { .. }
            | Self::ListAgents
            | Self::SetAgentState { .. }
            | Self::StartWokenTurn { .. }
            | Self::ReportTurnFailure { .. }
            | Self::ReportSubagent { .. }
            | Self::ReportSubagentRequest { .. }
            | Self::ReportWakeEnd { .. }
            | Self::ReportProxyRetry { .. }
            | Self::ReportUpstreamReset { .. }
            | Self::ReportUpstreamLimit { .. }
            | Self::ReportUpstreamAnswered { .. }
            | Self::ReportInput { .. }
            | Self::ReportTyping { .. }
            | Self::ReportAttach { .. }
            | Self::AwaitAgentWake { .. }
            | Self::AgentUpdates { .. }
            | Self::WaitAgentUpdates { .. }
            | Self::UpdatesWaiting { .. }
            | Self::GitCredential { .. }
            | Self::AcknowledgePrUpdates { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::SetAgentState { workspace, .. }
            | Self::StartWokenTurn { workspace }
            | Self::ReportTurnFailure { workspace, .. }
            | Self::ReportProxyRetry { workspace, .. }
            | Self::ReportUpstreamReset { workspace, .. }
            | Self::ReportUpstreamLimit { workspace, .. }
            | Self::ReportUpstreamAnswered { workspace, .. }
            | Self::ReportInput { workspace, .. }
            | Self::ReportTyping { workspace, .. }
            | Self::AwaitAgentWake { workspace, .. } => Some(Target::Space(workspace)),
            Self::SetProgress { space, .. }
            | Self::Progress { space }
            | Self::Sleep { space, .. }
            | Self::ReportSubagent { space, .. }
            | Self::ReportSubagentRequest { space, .. }
            | Self::ReportWakeEnd { space, .. }
            | Self::ReportAttach { space }
            | Self::SpaceAgents { space }
            | Self::InterruptTurn { space, .. }
            | Self::RestartAgent { space, .. }
            | Self::CancelRestart { space }
            | Self::ResumeAgent { space, .. }
            | Self::AgentSession { space }
            | Self::AgentUpdates { space }
            | Self::WaitAgentUpdates { space }
            | Self::UpdatesWaiting { space }
            | Self::AcknowledgePrUpdates { space, .. } => Some(Target::Space(space)),
            Self::GitCredential { .. } | Self::ListAgents => None,
        }
    }
}
