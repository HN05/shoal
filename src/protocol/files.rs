use serde::{Deserialize, Serialize};

/// Requests about the files attached to workspaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::enum_variant_names,
    reason = "variants are named as their requests are spelled on the wire"
)]
pub enum FileMethod {
    /// The files attached to a workspace or chat, oldest first.
    ListFiles { workspace: String },
    /// Attach copies of files to a workspace or chat without a message.
    AddFiles {
        workspace: String,
        /// The files by absolute path, which the daemon copies.
        files: Vec<crate::daemon::workspace_files::NewFile>,
        /// The chat whose agent sends the request, which attaches them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_chat: Option<String>,
    },
    /// Delete the attached files of a workspace or chat, only those with one
    /// of `names` and the records `ids` when there are any; without one,
    /// those of every workspace without a running agent or command and of
    /// every ended chat.
    RemoveFiles {
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        names: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ids: Vec<i64>,
    },
}
