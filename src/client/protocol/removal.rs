use crate::client::removal::UnsavedWork;
use serde::{Deserialize, Serialize};

/// Requests that stop, remove, clean up, export and restore workspaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalMethod {
    /// The workspaces `stop --all` stops.
    StopTargets,
    StopWorkspace {
        workspace: String,
        /// The user checked that executions Shoal cannot prove ended did.
        #[serde(default)]
        acknowledge_unproven: bool,
    },
    /// Refuse now what stopping `workspace` would refuse, so a batch checks
    /// every target before stopping any.
    CheckStop { workspace: String },
    /// Stop the untracked processes of the workspace or chat `space` that
    /// `selection` takes, other than process `requester` and the processes
    /// that started it.
    StopProcesses {
        space: String,
        selection: crate::client::processes::Selection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requester: Option<u32>,
    },
    CheckRemoval {
        workspace: String,
        #[serde(default)]
        include_changes: bool,
    },
    RemoveWorkspace {
        workspace: String,
        unsaved: UnsavedWork,
    },
    /// Remove idle and merged cleanup candidates without waiting for their
    /// idle delay, and empty shared caches nothing is using.
    Cleanup {
        dry_run: bool,
        #[serde(flatten)]
        selection: crate::daemon::cleanup::Selection,
    },
    /// Removed workspaces that can still be restored.
    RemovedWorkspaces,
    /// Open a removed workspace again from its removal record.
    RestoreWorkspace { workspace: String },
    /// End a space here, removing a workspace or stopping a chat's agent,
    /// or take a removed workspace, and write it for another Shoal to the
    /// new file `file`.
    ExportSpace {
        space: String,
        unsaved: UnsavedWork,
        file: std::path::PathBuf,
    },
    /// Record a workspace another Shoal exported as removed here, with its
    /// checkpoint fetched from `bundle`, and open it again.
    ImportWorkspace {
        repository: crate::repository::Selector,
        bundle: Option<std::path::PathBuf>,
        prerequisite: Option<String>,
        workspace: Box<crate::model::RemovedWorkspace>,
    },
}
