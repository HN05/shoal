//! Files attached to a workspace or chat: Shoal's copies and how they came.
use shoal_protocol::files::{AttachedFile, NewFile};

use crate::connection::PreparedPages;
use shoal_protocol::protocol::FileMethod;
use shoal_protocol::space_names::SpaceSelector;

/// The files attached to `space`.
pub fn list(space: SpaceSelector) -> PreparedPages<AttachedFile> {
    PreparedPages::new(FileMethod::ListFiles { space })
}

/// Attach copies of `files` to `space`.
pub fn add(space: SpaceSelector, files: Vec<NewFile>) -> PreparedPages<AttachedFile> {
    PreparedPages::new(FileMethod::AddFiles { space, files })
}

/// Delete the files of `space`, or of every space the caller reaches, that
/// `names` and `ids` select.
pub fn remove(
    space: Option<SpaceSelector>,
    names: Vec<String>,
    ids: Vec<i64>,
) -> PreparedPages<AttachedFile> {
    PreparedPages::new(FileMethod::RemoveFiles { space, names, ids })
}
