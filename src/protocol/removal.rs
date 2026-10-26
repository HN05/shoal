use crate::removal::UnsavedWork;
use serde::{Deserialize, Serialize};

/// Requests that stop, remove, clean up, export and restore workspaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalMethod {
    StopWorkspace {
        workspace: String,
        /// The user checked that executions Shoal cannot prove ended did.
        #[serde(default)]
        acknowledge_unproven: bool,
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
    /// Remove a workspace, or take a removed one, and write it for another
    /// Shoal to the new file `file`.
    ExportWorkspace {
        workspace: String,
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
