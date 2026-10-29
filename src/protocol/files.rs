use serde::{Deserialize, Serialize};

/// Requests about the files attached to workspaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMethod {
    /// The files attached to a workspace, oldest first.
    ListFiles { workspace: String },
}
