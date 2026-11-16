//! Packages of spaces `shoal move` carries between machines: what one holds.
use shoal_protocol::transfer::{ExportedSpace, SpaceImport};
use std::path::PathBuf;

use anyhow::Result;

use crate::connection::{Daemon, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    model::RestoredSpace,
    protocol::{RemovalMethod, WorkspaceMethod},
    removal::{RemovalResult, UnsavedWork},
};

use shoal_ids::checkpoint_numbers::CheckpointNumber;

/// End `space`, deleting only the `unsaved` work the caller saw, and
/// package it into `file`.
pub async fn export(
    daemon: &impl Daemon,
    space: SpaceSelector,
    unsaved: UnsavedWork,
    file: PathBuf,
) -> Result<ExportedSpace> {
    request(
        daemon,
        RemovalMethod::ExportSpace {
            space,
            unsaved,
            file,
        },
    )
    .await
}

/// Stop the agents of `space`, saving their sessions, and package it into
/// `file` for a move, leaving it here until [`end_moved`] ends it.
pub async fn export_moving(
    daemon: &impl Daemon,
    space: SpaceSelector,
    file: PathBuf,
) -> Result<ExportedSpace> {
    request(daemon, RemovalMethod::ExportMovingSpace { space, file }).await
}

/// End `space`, which another Shoal imported, deleting only the `unsaved`
/// work the caller saw: a workspace only while it still has what its
/// exported `checkpoint` keeps. Returns a workspace's removal.
pub async fn end_moved(
    daemon: &impl Daemon,
    space: SpaceSelector,
    unsaved: UnsavedWork,
    checkpoint: Option<CheckpointNumber>,
) -> Result<Option<RemovalResult>> {
    let method = RemovalMethod::EndMovedSpace {
        space,
        unsaved,
        checkpoint,
    };
    request(daemon, method).await
}

/// Package a copy of `space` into `file`, leaving it running: a workspace
/// at its checkpoint `checkpoint`, or at a new one.
pub async fn copy(
    daemon: &impl Daemon,
    space: SpaceSelector,
    checkpoint: Option<CheckpointNumber>,
    file: PathBuf,
) -> Result<ExportedSpace> {
    request(
        daemon,
        WorkspaceMethod::CopySpace {
            space,
            checkpoint,
            file,
        },
    )
    .await
}

/// Record the space `import` packages as removed and open it again.
pub async fn import(daemon: &impl Daemon, import: SpaceImport) -> Result<RestoredSpace> {
    request(daemon, RemovalMethod::ImportSpace(Box::new(import))).await
}
