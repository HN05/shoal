use serde::{Deserialize, Serialize};

/// Requests about the files attached to workspaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::enum_variant_names,
    reason = "variants are named as their requests are spelled on the wire"
)]
pub enum FileMethod {
    /// The files attached to a workspace, oldest first.
    ListFiles { workspace: String },
    /// Attach copies of files to a workspace without a message.
    AddFiles {
        workspace: String,
        /// Absolute paths of the files, which the daemon copies.
        files: Vec<std::path::PathBuf>,
    },
    /// Delete a workspace's attached files, only those with file name `name`
    /// when given; without a workspace, those of every workspace without a
    /// running agent or command.
    RemoveFiles {
        workspace: Option<String>,
        name: Option<String>,
    },
}
