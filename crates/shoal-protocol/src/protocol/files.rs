use super::{Forwarding, Lifetime, Target};
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests about the files attached to spaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMethod {
    /// The files attached to a space, oldest first.
    ListFiles { space: SpaceSelector },
    /// The bytes of the file `id` attached to a space, read on the machine
    /// that keeps its copy: answered with
    /// [`super::Body::FileBytes`] frames as the daemon reads the copy, the
    /// last [`crate::files::FileBytes::End`].
    ReadFile { space: SpaceSelector, id: i64 },
    /// The directory, marked as Shoal's, where this machine keeps the
    /// copies a caller here fetches of the files of `space`: in its
    /// repository's directory when this machine has the repository, else
    /// in `root_dir`.
    FetchedDirectory { space: SpaceSelector },
    /// Attach copies of files to a space without a message.
    AddFiles {
        space: SpaceSelector,
        /// The files by absolute path, which the daemon copies.
        files: Vec<crate::files::NewFile>,
    },
    /// Delete the attached files of a space, only those `names` and `ids`
    /// select when there are any. Without a space, it deletes those of every
    /// workspace without a running agent or command and of every removed
    /// space.
    RemoveFiles {
        space: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        names: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ids: Vec<i64>,
    },
}

impl FileMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::ListFiles { .. } | Self::ReadFile { .. } => true,
            Self::FetchedDirectory { .. } | Self::AddFiles { .. } | Self::RemoveFiles { .. } => {
                false
            }
        }
    }
}

impl FileMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::ReadFile { .. } => Lifetime::Open,
            Self::ListFiles { .. }
            | Self::FetchedDirectory { .. }
            | Self::AddFiles { .. }
            | Self::RemoveFiles { .. } => Lifetime::Reply,
        }
    }

    /// Attaching reads the files where the caller runs, and fetched copies
    /// are kept there; the rest read or delete the space's records, or read
    /// a copy's bytes as its machine reads them.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ListFiles { .. } | Self::RemoveFiles { .. } => Forwarding::Online,
            Self::ReadFile { .. } => Forwarding::Streamed,
            Self::AddFiles { .. } | Self::FetchedDirectory { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ListFiles { space }
            | Self::ReadFile { space, .. }
            | Self::RemoveFiles {
                space: Some(space), ..
            } => Some(Target::Files(space)),
            Self::AddFiles { space, .. } => Some(Target::Space(space)),
            // Where this machine keeps what a caller here fetches.
            Self::RemoveFiles { space: None, .. } | Self::FetchedDirectory { .. } => None,
        }
    }
}
