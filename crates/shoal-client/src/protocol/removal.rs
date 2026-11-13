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

/// What a stop at the user's request does with the workspace's processes
/// Shoal does not track, which nothing saves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UntrackedProcesses {
    /// Stop them, as `shoal stop` does, other than process `requester` and
    /// the processes that started it.
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requester: Option<u32>,
    },
    /// Leave them running, as `shoal move root` does, which starts only the
    /// stopped agents and commands again.
    Keep,
}

/// Requests that stop spaces, and remove, clean up, export and restore
/// workspaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalMethod {
    /// The workspaces and chats `stop --all` stops.
    StopTargets,
    /// Stop the agents and commands of the workspace or chat `space`,
    /// saving them for `shoal restore`.
    StopSpace {
        space: SpaceSelector,
        /// The user checked that executions Shoal cannot prove ended did.
        #[serde(default)]
        acknowledge_unproven: bool,
        /// What becomes of the agent launches waiting to start in it, which
        /// the answer lists when the stop cancels them.
        launches: QueuedLaunches,
        /// What becomes of its untracked processes, which the answer counts.
        processes: UntrackedProcesses,
    },
    /// Refuse now what stopping the workspace or chat `space` would refuse,
    /// so a batch checks every target before stopping any.
    CheckStop { space: SpaceSelector },
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
    /// Remove `workspace` as `RemoveWorkspace` does, answering once removal
    /// has reserved it and passed the checks it makes before changing
    /// anything, while the rest goes on in the background.
    StartRemoval {
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
    /// Delete the agents' session files the restore that opened `workspace`
    /// returned, once the caller has put them in place.
    DropRestoredSessions { workspace: SpaceSelector },
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
    /// new workspace on a branch of its own. The daemon keeps the agents'
    /// `session_files` with the record.
    ImportWorkspace {
        repository: crate::repository::Selector,
        bundle: Option<std::path::PathBuf>,
        prerequisite: Option<String>,
        workspace: Box<crate::model::RemovedWorkspace>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        copy: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        session_files: Vec<crate::transfer::CarriedSessionFile>,
    },
}
