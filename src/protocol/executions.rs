use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::{daemon::workspace::ExecutionKind, process::identity::Identity};

/// Requests that run tracked commands in a workspace.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMethod {
    /// Long-lived: the connection stays open for the execution's lifetime.
    /// `agent` names a Shoal agent shortcut whose exit the user is told about.
    Execute {
        workspace: String,
        wrapper: Identity,
        kind: ExecutionKind,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        recover: bool,
        /// The wrapper keeps its command running when the daemon goes away
        /// and returns with [`Reattachment::Reattach`].
        #[serde(default)]
        reattach: bool,
        /// The agent queue entry whose slot this agent takes over.
        #[serde(default)]
        queue_entry: Option<String>,
        /// The demo this command runs, recorded with its execution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        demo: Option<Box<crate::demos::Launch>>,
        /// Where the wrapper runs in a terminal host.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Box<crate::sessions::Origin>>,
    },
    /// Verify that this worker belongs to an unscoped caller's landing execution.
    CheckLanding,
    /// Connected agents, and executions whose wrapper is gone, which may run one.
    ListAgents,
    /// Stop the agent of `execution` and start its session again in its
    /// terminal, answering once the restarted agent runs. With `join`, only
    /// wait for the restart in progress, which may have ended already.
    /// `run` names the `shoal restart` run that asks.
    RestartAgent {
        execution: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        join: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<String>,
    },
}

impl ExecutionMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::Execute { .. } => Lifetime::Open,
            // A restart waits for requests of the wrapper it restores.
            Self::RestartAgent { .. } => Lifetime::Open,
            Self::CheckLanding | Self::ListAgents => Lifetime::Reply,
        }
    }
}
