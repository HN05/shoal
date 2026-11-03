use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::client::{executions::ExecutionKind, processes::Identity};

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
        /// The model and effort the agent runs with.
        #[serde(default, skip_serializing_if = "crate::agent::ModelChoice::is_empty")]
        model: crate::agent::ModelChoice,
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
        origin: Option<Box<crate::client::sessions::Origin>>,
        /// How a merge runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merge: Option<Box<crate::model::MergeOptions>>,
    },
    /// Verify that this worker belongs to an unscoped caller's merge execution.
    CheckMerge,
    /// Connected agents, and executions whose wrapper is gone, which may run one.
    ListAgents,
    /// Stop the agent of `execution` and start its session again in its
    /// terminal, answering once the restarted agent runs. With `join`, only
    /// wait for that restart the run selected, which may have ended already.
    /// `run` names the `shoal restart` run that asks.
    RestartAgent {
        execution: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        join: Option<crate::model::PendingRestart>,
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
            Self::CheckMerge | Self::ListAgents => Lifetime::Reply,
        }
    }
}
