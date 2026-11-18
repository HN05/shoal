//! Packages of spaces `shoal move` carries between machines: what one holds.
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::transfer::{Carrying, ExportedSpace, SpaceImport};
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;

use crate::connection::{Daemon, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    model::RestoredSpace,
    protocol::{RemovalMethod, WorkspaceMethod},
    removal::{RemovalResult, UnsavedWork},
};

use shoal_ids::checkpoint_numbers::CheckpointNumber;

/// End `space` with the spaces below it that `carrying` carries,
/// deleting only the `unsaved` work the caller saw, and package them into
/// `file`.
pub async fn export(
    daemon: &impl Daemon,
    space: SpaceSelector,
    unsaved: UnsavedWork,
    file: PathBuf,
    carrying: Carrying,
) -> Result<ExportedSpace> {
    let method = RemovalMethod::ExportSpace {
        space,
        unsaved,
        file,
        carrying,
    };
    request(daemon, method).await
}

/// Stop the agents of `space` and of the spaces below it that `carrying`
/// carries, saving their sessions, and package them into `file` for a
/// move, leaving them here until [`end_moved`] ends them.
pub async fn export_moving(
    daemon: &impl Daemon,
    space: SpaceSelector,
    file: PathBuf,
    carrying: Carrying,
) -> Result<ExportedSpace> {
    let method = RemovalMethod::ExportMovingSpace {
        space,
        file,
        carrying,
    };
    request(daemon, method).await
}

/// End `space` with the spaces below it that `carrying` carries, which
/// another Shoal imported, deleting only the `unsaved` work the caller
/// saw: each workspace only while it still has what its exported
/// checkpoint in `checkpoints` keeps. Returns the removal.
pub async fn end_moved(
    daemon: &impl Daemon,
    space: SpaceSelector,
    unsaved: UnsavedWork,
    checkpoints: BTreeMap<SpaceId, CheckpointNumber>,
    carrying: Carrying,
) -> Result<RemovalResult> {
    let method = RemovalMethod::EndMovedSpace {
        space,
        unsaved,
        checkpoints,
        carrying,
    };
    request(daemon, method).await
}

/// Package a copy of `space`, with copies of the spaces below it that
/// `carrying` carries, into `file`, leaving them running: a workspace at
/// its checkpoint `checkpoint`, or at a new one.
pub async fn copy(
    daemon: &impl Daemon,
    space: SpaceSelector,
    checkpoint: Option<CheckpointNumber>,
    file: PathBuf,
    carrying: Carrying,
) -> Result<ExportedSpace> {
    let method = WorkspaceMethod::CopySpace {
        space,
        checkpoint,
        file,
        carrying,
    };
    request(daemon, method).await
}

/// Record the space `import` packages as removed and open it again.
pub async fn import(daemon: &impl Daemon, import: SpaceImport) -> Result<RestoredSpace> {
    request(daemon, RemovalMethod::ImportSpace(Box::new(import))).await
}
