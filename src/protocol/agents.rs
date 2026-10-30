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
        workspace: String,
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
        workspace: String,
    },
    /// [`Self::SetAgentState`] idle for a turn `failure` ended, which may
    /// schedule a retry of the turn. An unscoped caller, such as the wrapper
    /// whose proxy ended the turn, names the reporting `execution`; a scoped
    /// one reports for its own.
    ReportTurnFailure {
        workspace: String,
        failure: crate::state::TurnFailure,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution: Option<String>,
        #[serde(default, skip_serializing_if = "TurnEnd::is_empty")]
        turn_end: TurnEnd,
    },
    /// The retry the proxy of `execution` waits to make, which its wrapper
    /// reports; `None` once the proxy succeeded or gave up.
    ReportProxyRetry {
        workspace: String,
        execution: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry: Option<crate::daemon::workspace::proxy_retries::ProxyRetry>,
    },
    /// The upstream of `execution`'s proxy refuses requests until Unix
    /// second `resets_at`, such as a usage limit's reset, which its wrapper
    /// reports so a retry of the agent's turn waits past it.
    ReportUpstreamReset {
        workspace: String,
        execution: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resets_at: Option<i64>,
    },
    /// Whether a terminal shows the agent `execution` runs, which its
    /// wrapper reads from the place it runs in and reports again
    /// periodically, so a restarted daemon learns it too.
    ReportAttached {
        workspace: String,
        execution: String,
        attached: bool,
    },
    /// Long-lived: answers once whether the workspace's agent is idle with
    /// undelivered messages or queued item updates matches `due`; with
    /// `interrupted`, only while an interrupt ended the last turn of the
    /// agent that execution runs, which its hook does not wake then.
    AwaitAgentWake {
        workspace: String,
        due: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interrupted: Option<String>,
    },
    /// The agents connected in the workspace, with their turns and places.
    WorkspaceAgents {
        workspace: String,
    },
    /// An interrupt the caller typed ended the turn of the agent `execution`
    /// runs in the workspace, which leaves it idle.
    InterruptTurn {
        workspace: String,
        execution: String,
    },
    /// The session of the workspace's agent that a fork starts from: the
    /// caller's own agent there, else its only one.
    AgentSession {
        workspace: String,
    },
    /// A message from the user to a workspace's agents.
    SendAgentMessage {
        workspace: String,
        message: String,
        /// Files to attach by absolute path, which the daemon copies.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::daemon::workspace_files::NewFile>,
    },
    /// [`Self::SendAgentMessage`] to every workspace of the repository with
    /// a running agent, answered with the workspaces it reached.
    SendRepositoryAgentMessage {
        repository: crate::repository::Selector,
        message: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::daemon::workspace_files::NewFile>,
    },
    /// The workspace's undelivered agent messages, oldest first.
    AgentMessages {
        workspace: String,
    },
    MarkAgentMessagesDelivered {
        workspace: String,
        ids: Vec<i64>,
    },
    /// The workspace's undelivered agent messages and queued item updates,
    /// without waiting or polling the forge; acknowledge them with
    /// [`Self::AcknowledgePrUpdates`]. It replaces a running watch.
    AgentUpdates {
        workspace: String,
    },
    AcknowledgePrUpdates {
        workspace: String,
        deliveries: Vec<String>,
        /// Agent messages the watch printed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        messages: Vec<i64>,
    },
}

/// Whether the turn a wake hook starts goes ahead: not when a stop of the
/// agent came first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WokenTurn {
    pub starts: bool,
}

/// What an agent reports when a turn ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEnd {
    /// How long its prompt cache lives after the turn, as its own settings
    /// give it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_secs: Option<u64>,
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
            Self::AwaitAgentWake { .. } => Lifetime::Resumed,
            _ => Lifetime::Reply,
        }
    }
}
