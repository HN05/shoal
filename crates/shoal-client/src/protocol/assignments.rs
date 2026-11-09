use crate::execution_ids::ExecutionId;
use crate::space_ids::SpaceId;
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

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
        /// The chat whose agent sends the request, which attaches them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_chat: Option<SpaceId>,
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
    /// Forget the workspace's stopped work, only `execution`'s when given,
    /// or else its last session.
    DiscardStoppedWork {
        workspace: SpaceSelector,
        execution: Option<ExecutionId>,
    },
    /// Make `change` to the permissions each of the stopped `agents` and
    /// saved `chats` was given, which its restore runs under, once none of
    /// them is refused.
    ChangeStoppedPermissions {
        agents: Vec<SavedAgent>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        chats: Vec<SpaceSelector>,
        change: crate::config::permissions::PermissionChange,
    },
}

/// A workspace's stopped agent or last session, by its execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedAgent {
    pub workspace: SpaceSelector,
    pub execution: String,
}
