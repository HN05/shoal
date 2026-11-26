use serde::{Deserialize, Serialize};

use super::{Forwarding, Lifetime, Target};
use crate::space_names::SpaceSelector;
use crate::{executions::ExecutionKind, processes::Identity};

/// Requests that run tracked commands, agents included, in a space.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "a request, sent and read once, as large as its operation"
)]
pub enum ExecutionMethod {
    /// Long-lived: the connection stays open for the execution's lifetime.
    /// `agent` names the agent the command runs, whose exit the user is told about.
    Execute {
        space: SpaceSelector,
        wrapper: Identity,
        kind: ExecutionKind,
        #[serde(default)]
        agent: Option<crate::agent::Agent>,
        #[serde(default, skip_serializing_if = "crate::agent::ModelChoice::is_empty")]
        model: crate::agent::ModelChoice,
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
        /// What made a restore its wrapper runs in the caller's terminal,
        /// which counts as the user's interaction with the agent when
        /// manual and unscoped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<crate::launches::LaunchSource>,
        /// The wrapper keeps its command running when the daemon goes away
        /// and returns with [`Reattachment::Reattach`](super::Reattachment::Reattach).
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merge: Option<Box<crate::model::MergeOptions>>,
    },
    /// Verify that this worker belongs to an unscoped caller's merge execution.
    CheckMerge,
    /// Verify that this worker belongs to an unscoped caller's unmerge execution.
    CheckUnmerge,
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
            Self::CheckMerge | Self::CheckUnmerge | Self::SessionSignalled { .. } => {
                Lifetime::Reply
            }
        }
    }
}

impl ExecutionMethod {
    /// Each is the caller's own execution.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::Execute { .. }
            | Self::CheckMerge
            | Self::CheckUnmerge
            | Self::SessionSignalled { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::Execute { space, .. } => Some(Target::Space(space)),
            Self::CheckMerge | Self::CheckUnmerge | Self::SessionSignalled { .. } => None,
        }
    }
}
