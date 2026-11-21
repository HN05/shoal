use super::Forwarding;
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;

/// Requests about a space's assignment: completion, ready marks and a
/// workspace's stopped work.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentMethod {
    /// Record the work of a workspace or running chat as done.
    MarkDone {
        space: SpaceSelector,
        #[serde(flatten)]
        done: crate::assignments::Done,
    },
    /// Withdraw the completion of a workspace or running chat.
    UndoDone { space: SpaceSelector },
    MarkReady {
        space: SpaceSelector,
        selection: crate::forge::items::link::Selection,
    },
    /// `draft` overrides `[pr] draft_on_unready` when set.
    ClearReady {
        space: SpaceSelector,
        selection: crate::forge::items::link::Selection,
        draft: Option<bool>,
    },
    /// Discard the space's stopped agent, last sessions and stopped
    /// commands, only `execution`'s when given.
    DiscardStoppedAgent {
        space: SpaceSelector,
        execution: Option<ExecutionId>,
    },
    /// Make `change` to the permissions each of the stopped `agents` and
    /// saved `chats` was given, which its restore runs under, and the
    /// launch waiting for the failed setup of each of the `launches`
    /// workspaces, which it starts under, once none of them is refused.
    ChangeStoppedPermissions {
        agents: Vec<SavedAgent>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        launches: Vec<SpaceSelector>,
        change: crate::config::permissions::PermissionChange,
    },
}

/// A workspace's stopped agent or last session, by its execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedAgent {
    pub workspace: SpaceSelector,
    pub execution: ExecutionId,
}

impl AssignmentMethod {
    /// Each changes the state of the space it names.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::MarkDone { .. }
            | Self::UndoDone { .. }
            | Self::MarkReady { .. }
            | Self::ClearReady { .. }
            | Self::DiscardStoppedAgent { .. }
            | Self::ChangeStoppedPermissions { .. } => Forwarding::Online,
        }
    }
}
