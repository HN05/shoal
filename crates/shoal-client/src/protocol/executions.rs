use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::space_names::SpaceSelector;
use crate::{executions::ExecutionKind, processes::Identity};

/// Requests that run tracked commands in a workspace.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMethod {
    /// Long-lived: the connection stays open for the execution's lifetime.
    /// `agent` names a Shoal agent shortcut whose exit the user is told about.
    Execute {
        workspace: SpaceSelector,
        wrapper: Identity,
        kind: ExecutionKind,
        #[serde(default)]
        agent: Option<String>,
        /// The model and effort the agent runs with.
        #[serde(default, skip_serializing_if = "crate::agent::ModelChoice::is_empty")]
        model: crate::agent::ModelChoice,
        /// The mode the agent started in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        /// The `--permission` narrowing of the mode's permissions.
        #[serde(
            default,
            skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
        )]
        narrowing: crate::config::permissions::PermissionsConfig,
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
        /// How a merge runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merge: Option<Box<crate::model::MergeOptions>>,
    },
    /// Verify that this worker belongs to an unscoped caller's merge execution.
    CheckMerge,
    /// Verify that this worker belongs to an unscoped caller's unmerge execution.
    CheckUnmerge,
    /// Connected agents, and executions whose wrapper is gone, which may run one.
    ListAgents,
    /// Stop the agent of `execution` and start its session again in a new
    /// wrapper, or in its own terminal when `in_place`, answering once the
    /// restarted agent runs with the place of the wrapper that runs it. With `join`, only wait for that restart the run
    /// selected, which may have ended already, as it was asked. `run` names
    /// the `shoal restart` run that asks.
    RestartAgent {
        execution: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        join: Option<crate::model::PendingRestart>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        in_place: bool,
    },
}

impl ExecutionMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::Execute { .. } => Lifetime::Open,
            // A restart waits for requests of the wrapper it restores.
            Self::RestartAgent { .. } => Lifetime::Open,
            Self::CheckMerge | Self::CheckUnmerge | Self::ListAgents => Lifetime::Reply,
        }
    }
}
