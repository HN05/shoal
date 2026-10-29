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
    /// Delete a workspace's attached files, only those with one of `names`
    /// and the records `ids` when there are any; without a workspace, those
    /// of every workspace without a running agent or command.
    RemoveFiles {
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        names: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ids: Vec<i64>,
    },
}
