use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

use super::Lifetime;

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
        session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_end: Option<TurnEnd>,
    },
    /// [`Self::SetAgentState`] working for the turn a wake hook starts with
    /// an update, unless a stop of the caller's agent came first, which
    /// leaves the update for its restore. Answers whether the turn starts.
    StartWokenTurn {
        workspace: SpaceSelector,
    },
    /// [`Self::SetAgentState`] idle for a turn `failure` ended, which may
    /// schedule a retry of the turn. An unscoped caller, such as the wrapper
    /// whose proxy ended the turn, names the reporting `execution`; a scoped
    /// one reports for its own.
    ReportTurnFailure {
        workspace: SpaceSelector,
        failure: crate::state::TurnFailure,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution: Option<String>,
        #[serde(default, skip_serializing_if = "TurnEnd::is_empty")]
        turn_end: TurnEnd,
    },
    /// The wake hook of the space's agent ended for `ended` without
    /// starting the agent's turn, which the daemon logs so that a missed
    /// wake can be traced.
    ReportWakeEnd {
        space: SpaceSelector,
        ended: WakeEnd,
    },
    /// A turn-state report of the caller's agent that failed with `error`,
    /// which leaves the state shown as last reported.
    ReportStateFailure {
        workspace: SpaceSelector,
        error: String,
    },
    /// The retry the proxy of `execution` waits to make, which its wrapper
    /// reports; `None` once the proxy succeeded or gave up.
    ReportProxyRetry {
        workspace: SpaceSelector,
        execution: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry: Option<crate::executions::ProxyRetry>,
    },
    /// The upstream of `execution`'s proxy refuses requests until Unix
    /// second `resets_at`, such as a usage limit's reset, which its wrapper
    /// reports so a retry of the agent's turn waits past it.
    ReportUpstreamReset {
        workspace: SpaceSelector,
        execution: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resets_at: Option<i64>,
    },
    /// Whether a terminal shows the agent `execution` runs, which its
    /// wrapper reads from the place it runs in and reports again
    /// periodically, so a restarted daemon learns it too.
    ReportAttached {
        workspace: SpaceSelector,
        execution: String,
        attached: bool,
    },
    /// Long-lived: answers once whether the workspace's agent is idle with
    /// undelivered messages or queued item updates matches `due`; with
    /// `hooked`, the execution whose agent a hook of its own wakes, only
    /// while that hook cannot: after an interrupt ended its last turn, or
    /// once no wait for the workspace's updates has run for a few minutes.
    AwaitAgentWake {
        workspace: SpaceSelector,
        due: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hooked: Option<String>,
    },
    /// The agents connected in the workspace, with their turns and places.
    WorkspaceAgents {
        workspace: SpaceSelector,
    },
    /// An interrupt the caller typed ended the turn of the agent `execution`
    /// runs in the workspace, which leaves it idle.
    InterruptTurn {
        workspace: SpaceSelector,
        execution: String,
    },
    /// The session of the workspace's agent that a fork starts from: the
    /// caller's own agent there, else its only one.
    AgentSession {
        workspace: SpaceSelector,
    },
    /// A message to the agents of a workspace or running chat.
    SendAgentMessage {
        space: SpaceSelector,
        message: String,
        /// Files to attach by absolute path, which the daemon copies.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::files::NewFile>,
        /// The chat whose agent sends the message, which attaches its files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_chat: Option<String>,
    },
    /// [`Self::SendAgentMessage`] to every workspace and chat with a running
    /// agent, of one repository when it names one, answered with the spaces
    /// it reached.
    SendAllAgentMessage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
        message: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::files::NewFile>,
        /// The chat whose agent sends the message, which attaches its files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_chat: Option<String>,
    },
    /// The undelivered messages of the agents of a workspace or running
    /// chat, oldest first.
    AgentMessages {
        space: SpaceSelector,
    },
    MarkAgentMessagesDelivered {
        space: SpaceSelector,
        ids: Vec<i64>,
    },
    /// The undelivered agent messages and queued item updates of a
    /// workspace or running chat, without waiting or polling the forge;
    /// acknowledge them with [`Self::AcknowledgePrUpdates`]. It replaces a
    /// running watch.
    AgentUpdates {
        space: SpaceSelector,
    },
    /// Long-lived: what [`Self::AgentUpdates`] answers, once there is any.
    WaitAgentUpdates {
        space: SpaceSelector,
    },
    AcknowledgePrUpdates {
        space: SpaceSelector,
        deliveries: Vec<String>,
        /// Agent messages the watch printed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        messages: Vec<i64>,
        /// The PIDs of the leftovers the hook printed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        leftovers: Vec<u32>,
    },
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

impl AgentMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::AwaitAgentWake { .. } | Self::WaitAgentUpdates { .. } => Lifetime::Resumed,
            _ => Lifetime::Reply,
        }
    }
}
