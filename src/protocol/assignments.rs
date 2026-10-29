use serde::{Deserialize, Serialize};

/// Requests about a workspace's assignment: completion, ready marks and stopped work.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentMethod {
    /// `cleanup` requests cleanup even when `[done] cleanup` is false;
    /// `integrated` records that another branch has HEAD's work under other
    /// commits.
    WorkspaceDone {
        workspace: String,
        cleanup: bool,
        #[serde(default)]
        integrated: bool,
    },
    WorkspaceUndone {
        workspace: String,
    },
    MarkReady {
        workspace: String,
        selection: crate::forge::items::link::Selection,
    },
    /// `draft` overrides `[unready] draft` when set.
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
