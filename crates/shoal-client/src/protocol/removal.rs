use crate::removal::UnsavedWork;
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// What a stop at the user's request does with the agent launches waiting
/// to start in the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedLaunches {
    /// Cancel them, as `shoal stop` does, so no agent starts in a workspace
    /// the user stopped.
    Cancel,
    /// Leave them queued, as `shoal daemon stop` and `shoal move root` do,
    /// which start the stopped work again later.
    Keep,
}

/// Requests that stop, remove, clean up, export and restore workspaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalMethod {
    /// The workspaces `stop --all` stops.
    StopTargets,
    StopWorkspace {
        workspace: SpaceSelector,
        /// The user checked that executions Shoal cannot prove ended did.
        #[serde(default)]
        acknowledge_unproven: bool,
        /// What becomes of the agent launches waiting to start in it, which
        /// the answer lists when the stop cancels them.
        launches: QueuedLaunches,
    },
    /// Refuse now what stopping `workspace` would refuse, so a batch checks
    /// every target before stopping any.
    CheckStop { workspace: SpaceSelector },
    /// Stop the untracked processes of the workspace or chat `space` that
    /// `selection` takes, other than process `requester` and the processes
    /// that started it.
    StopProcesses {
        space: SpaceSelector,
        selection: crate::processes::Selection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requester: Option<u32>,
    },
    CheckRemoval {
        workspace: SpaceSelector,
        #[serde(default)]
        include_changes: bool,
    },
    RemoveWorkspace {
        workspace: SpaceSelector,
        unsaved: UnsavedWork,
    },
    /// Remove idle and merged cleanup candidates without waiting for their
    /// idle delay, and empty shared caches nothing is using.
    Cleanup {
        dry_run: bool,
        #[serde(flatten)]
        selection: crate::cleanup::Selection,
    },
    /// Removed workspaces that can still be restored.
    RemovedWorkspaces,
    /// Open a removed workspace again from its removal record.
    RestoreWorkspace { workspace: SpaceSelector },
    /// End a space here, removing a workspace or stopping a chat's agent,
    /// or take a removed workspace, and write it for another Shoal to the
    /// new file `file`.
    ExportSpace {
        space: SpaceSelector,
        unsaved: UnsavedWork,
        file: std::path::PathBuf,
    },
    /// Record a workspace another Shoal exported as removed here, with its
    /// checkpoint fetched from `bundle`, and open it again; a `copy` as a
    /// new workspace on a branch of its own.
    ImportWorkspace {
        repository: crate::repository::Selector,
        bundle: Option<std::path::PathBuf>,
        prerequisite: Option<String>,
        workspace: Box<crate::model::RemovedWorkspace>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        copy: bool,
    },
}
