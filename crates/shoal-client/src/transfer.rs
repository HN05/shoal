//! Packages of spaces `shoal move` carries between machines: what one holds.
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::transfer::{Carrying, ExportedSpace, ImportedPackage, PackageImport};
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncWriteExt};

use crate::connection::{Daemon, open_buffered, request};
use shoal_protocol::repository::Selector;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    protocol::{self, RemovalMethod, WorkspaceMethod},
    removal::{RemovalResult, UnsavedWork},
    transfer::PackageSource,
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

/// Import the package `import` names: the daemon records its spaces as
/// removed and opens its own again.
pub async fn import(daemon: &impl Daemon, import: PackageImport) -> Result<ImportedPackage> {
    request(daemon, RemovalMethod::ImportPackage(Box::new(import))).await
}

/// [`import`] the package `package` holds, into `repository` or the one
/// with its origin, keeping the agents' sessions as `sessions` says:
/// sent on the request's connection once the daemon asks for it.
pub async fn import_streamed(
    daemon: &impl Daemon,
    repository: Option<Selector>,
    sessions: Option<bool>,
    package: &mut (impl AsyncRead + Unpin),
) -> Result<ImportedPackage> {
    let import = PackageImport {
        package: PackageSource::Stream,
        repository,
        sessions,
    };
    let method = RemovalMethod::ImportPackage(Box::new(import));
    let (mut stream, asked) = open_buffered(daemon, method).await?;
    <()>::try_from(asked)?;
    let sent = async {
        tokio::io::copy(package, stream.get_mut()).await?;
        stream.get_mut().shutdown().await
    };
    let sent = sent.await;
    // A daemon that refuses the package answers before reading all of it.
    match protocol::read_response(&mut stream).await {
        Ok(response) => response.body.try_into(),
        Err(error) => match sent {
            Err(sending) => Err(sending).context("send the package"),
            Ok(()) => Err(error),
        },
    }
}
