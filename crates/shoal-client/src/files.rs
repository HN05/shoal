//! Files attached to a workspace or chat: Shoal's copies and how they came.
use shoal_protocol::files::{AttachedFile, NewFile};

use shoal_ids::space_ids::SpaceId;

use anyhow::Result;

use crate::connection::{Daemon, pages};
use shoal_protocol::protocol::FileMethod;
use shoal_protocol::space_names::SpaceSelector;

/// The files attached to `space`.
pub async fn list(daemon: &impl Daemon, space: SpaceSelector) -> Result<Vec<AttachedFile>> {
    pages(daemon, FileMethod::ListFiles { space }).await
}

/// Attach copies of `files` to `space`; the chat `sender_chat` names, when
/// its agent attaches them, attaches its own files.
pub async fn add(
    daemon: &impl Daemon,
    space: SpaceSelector,
    files: Vec<NewFile>,
    sender_chat: Option<SpaceId>,
) -> Result<Vec<AttachedFile>> {
    let method = FileMethod::AddFiles {
        space,
        files,
        sender_chat,
    };
    pages(daemon, method).await
}

/// Delete the files of `space`, or of every space the caller reaches, that
/// `names` and `ids` select.
pub async fn remove(
    daemon: &impl Daemon,
    space: Option<SpaceSelector>,
    names: Vec<String>,
    ids: Vec<i64>,
) -> Result<Vec<AttachedFile>> {
    pages(daemon, FileMethod::RemoveFiles { space, names, ids }).await
}
