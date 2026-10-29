use serde::{Deserialize, Serialize};

/// Requests about the files attached to workspaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMethod {
    /// The files attached to a workspace, oldest first.
    ListFiles { workspace: String },
    /// Attach copies of files to a workspace without a message.
    AddFiles {
        workspace: String,
        /// Absolute paths of the files, which the daemon copies.
        files: Vec<std::path::PathBuf>,
    },
}
