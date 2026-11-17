use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;

use super::Lifetime;
use crate::space_names::SpaceSelector;
use crate::{executions::ExecutionKind, processes::Identity};

/// Requests that run tracked commands in a space: a workspace's commands,
/// or a workspace's or a chat's agent.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMethod {
    /// Long-lived: the connection stays open for the execution's lifetime.
    /// `agent` names the agent the command runs, whose exit the user is told about.
    Execute {
        space: SpaceSelector,
        wrapper: Identity,
        kind: ExecutionKind,
        #[serde(default)]
        agent: Option<crate::agent::Agent>,
        /// The model and effort the agent runs with.
        #[serde(default, skip_serializing_if = "crate::agent::ModelChoice::is_empty")]
        model: crate::agent::ModelChoice,
        /// The mode the agent started in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<crate::config::modes::ModeName>,
        /// The permissions `--permission` gives the agent, whatever the mode's.
        #[serde(
            default,
            skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
        )]
        narrowing: crate::config::permissions::PermissionsConfig,
        /// The agent begins a new session, so its space's watches restart
        /// from what its items hold now.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        new_session: bool,
        #[serde(default)]
        recover: bool,
        /// The wrapper keeps its command running when the daemon goes away
        /// and returns with [`Reattachment::Reattach`].
        #[serde(default)]
        reattach: bool,
        /// The agent queue entry whose slot this agent takes over.
        #[serde(default)]
        queue_entry: Option<shoal_ids::queue_entry_ids::QueueEntryId>,
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
    /// the `shoal restart` run that asks. `change` changes the permissions the
    /// agent was given, which only a new wrapper starts it under.
    RestartAgent {
        execution: ExecutionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        join: Option<crate::model::PendingRestart>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        in_place: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        change: Option<crate::config::permissions::PermissionChange>,
    },
    /// `signal` ends the session of the caller's execution, whose wrapper
    /// stops its command once the daemon answers, after the daemon ended
    /// what the command runs elsewhere, such as a CI job's run on its forge.
    SessionSignalled { signal: SessionSignal },
}

/// A signal that ends an execution's session, as closing its terminal,
/// logging out or shutting down sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSignal {
    Hangup,
    Terminate,
}

impl std::fmt::Display for SessionSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Hangup => "SIGHUP",
            Self::Terminate => "SIGTERM",
        })
    }
}

impl ExecutionMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::Execute { .. } => Lifetime::Open,
            // A restart waits for requests of the wrapper it restores.
            Self::RestartAgent { .. } => Lifetime::Open,
            Self::CheckMerge
            | Self::CheckUnmerge
            | Self::ListAgents
            | Self::SessionSignalled { .. } => Lifetime::Reply,
        }
    }
}
