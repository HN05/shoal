use crate::client::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests about a space's assignment: completion, ready marks and a
/// workspace's stopped work.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentMethod {
    /// Record the work of a workspace or running chat as done. For a
    /// workspace, `cleanup` requests cleanup even when `[cleanup.done]
    /// enabled` is false and `integrated` records that another branch has
    /// HEAD's work under other commits; a chat has neither. `results` are
    /// files to hand back by absolute path, which the daemon copies.
    MarkDone {
        space: SpaceSelector,
        cleanup: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        integrated: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        results: Vec<crate::client::files::NewFile>,
        /// The chat whose agent sends the request, which attaches them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_chat: Option<String>,
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
        execution: Option<String>,
    },
}
