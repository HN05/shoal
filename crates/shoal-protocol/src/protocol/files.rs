use super::{Forwarding, Target};
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests about the files attached to spaces: workspaces and chats.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::enum_variant_names,
    reason = "variants are named as their requests are spelled on the wire"
)]
pub enum FileMethod {
    /// The files attached to a workspace or chat, oldest first.
    ListFiles { space: SpaceSelector },
    /// Attach copies of files to a workspace or chat without a message.
    AddFiles {
        space: SpaceSelector,
        /// The files by absolute path, which the daemon copies.
        files: Vec<crate::files::NewFile>,
    },
    /// Delete the attached files of a workspace or chat, only those with one
    /// of `names` and the records `ids` when there are any; without one,
    /// those of every workspace without a running agent or command and of
    /// every removed space.
    RemoveFiles {
        space: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        names: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ids: Vec<i64>,
    },
}

impl FileMethod {
    /// Attaching reads the files where the caller runs; the rest read or
    /// delete the space's records.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ListFiles { .. } | Self::RemoveFiles { .. } => Forwarding::Online,
            Self::AddFiles { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ListFiles { space }
            | Self::AddFiles { space, .. }
            | Self::RemoveFiles {
                space: Some(space), ..
            } => Some(Target::Space(space)),
            Self::RemoveFiles { space: None, .. } => None,
        }
    }
}
