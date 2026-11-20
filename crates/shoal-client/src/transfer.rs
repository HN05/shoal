//! Packages of spaces `shoal move` carries between machines: what one holds.
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::transfer::{Carrying, ExportedSpace, ImportedPackage, PackageImport};
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

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
    let (stream, asked) = open_buffered(daemon, method).await?;
    <()>::try_from(asked)?;
    let (read, answer) = exchange(package, stream).await;
    let imported = answer.and_then(|response| ImportedPackage::try_from(response.body));
    match read {
        Ok(()) => imported,
        Err(read) => imported.map_err(|refused| refused.context(format!("{read:#}"))),
    }
}

/// Send `package` to the daemon on `stream` as [`send_package`] does while
/// reading its answer, which ends the sending once it arrives, since a
/// daemon that refuses the package answers before reading all of it; the
/// error reading `package`, and the answer.
async fn exchange(
    package: &mut (impl AsyncRead + Unpin),
    stream: BufReader<UnixStream>,
) -> (Result<()>, Result<protocol::Response>) {
    let (answering, mut sending) = tokio::io::split(stream);
    let mut answering = BufReader::new(answering);
    // Never cancelled, so no part of the answer it reads is lost.
    let answer = protocol::read_response(&mut answering);
    tokio::pin!(answer);
    let read = tokio::select! {
        answer = &mut answer => return (Ok(()), answer),
        read = send_package(package, &mut sending) => read,
    };
    (read, answer.await)
}

/// Send what `package` holds to the daemon on `stream` until it ends, the
/// daemon stops reading or reading it fails, then shut the sending half, so
/// the daemon always reaches the end of what it received and answers; the
/// error reading `package`.
async fn send_package(
    package: &mut (impl AsyncRead + Unpin),
    stream: &mut (impl AsyncWrite + Unpin),
) -> Result<()> {
    let mut buffer = vec![0; 64 * 1024];
    let read = loop {
        let length = match package.read(&mut buffer).await {
            Ok(0) => break Ok(()),
            Ok(length) => length,
            Err(error) => break Err(error),
        };
        // The daemon stopped reading; its answer says why.
        if stream.write_all(&buffer[..length]).await.is_err() {
            break Ok(());
        }
    };
    // A daemon that hung up needs no end.
    let _ = stream.shutdown().await;
    read.context("read the package")
}

#[cfg(test)]
mod tests;
