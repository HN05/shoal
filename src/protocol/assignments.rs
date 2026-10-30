use serde::{Deserialize, Serialize};

/// Requests about a workspace's assignment: completion, ready marks and stopped work.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentMethod {
    /// `cleanup` requests cleanup even when `[cleanup.done] enabled` is false;
    /// `integrated` records that another branch has HEAD's work under other
    /// commits; `results` are absolute paths of files to hand back, which the
    /// daemon copies.
    WorkspaceDone {
        workspace: String,
        cleanup: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        integrated: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        results: Vec<std::path::PathBuf>,
    },
    WorkspaceUndone {
        workspace: String,
    },
    MarkReady {
        workspace: String,
        selection: crate::forge::items::link::Selection,
    },
    /// `draft` overrides `[pr] draft_on_unready` when set.
    ClearReady {
        workspace: String,
        selection: crate::forge::items::link::Selection,
        draft: Option<bool>,
    },
    /// Forget the workspace's stopped work, only `execution`'s when given,
    /// or else its last session.
    DiscardStoppedWork {
        workspace: String,
        execution: Option<String>,
    },
}
