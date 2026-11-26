use super::{Forwarding, Target};
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;

/// Requests about a space's assignment: completion, ready marks and a
/// workspace's stopped work.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentMethod {
    MarkDone {
        space: SpaceSelector,
        #[serde(flatten)]
        done: crate::assignments::Done,
    },
    UndoDone {
        space: SpaceSelector,
    },
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
    /// commands, leaving an agent a wrapper waits to restore, and its failed
    /// launch; only `execution`'s agent when given.
    DiscardStoppedAgent {
        space: SpaceSelector,
        execution: Option<ExecutionId>,
    },
    /// Apply `change` to the permissions that each of the stopped `agents`,
    /// the saved `chats` and the launches waiting for the failed setup of
    /// the `launches` workspaces was given. Changes none when one is
    /// refused.
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

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::MarkDone { space, .. }
            | Self::UndoDone { space }
            | Self::MarkReady { space, .. }
            | Self::ClearReady { space, .. }
            | Self::DiscardStoppedAgent { space, .. } => Some(Target::Space(space)),
            // It names several spaces.
            Self::ChangeStoppedPermissions { .. } => None,
        }
    }
}
