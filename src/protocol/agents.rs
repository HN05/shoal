use serde::{Deserialize, Serialize};

use super::Lifetime;

/// Requests between a workspace's agents and the daemon: their state, messages and updates.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMethod {
    /// The caller's execution, when scoped, is recorded as the reporter,
    /// and `session` as the agent session it resumes.
    SetAgentState {
        workspace: String,
        state: crate::state::AgentState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
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
    /// Long-lived: answers once whether the workspace's agent is idle with
    /// undelivered messages or queued item updates matches `due`.
    AwaitAgentWake {
        workspace: String,
        due: bool,
    },
    /// A message from the user to a workspace's agents.
    SendAgentMessage {
        workspace: String,
        message: String,
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

impl AgentMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::AwaitAgentWake { .. } => Lifetime::Resumed,
            _ => Lifetime::Reply,
        }
    }
}
