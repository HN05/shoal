//! Packages of spaces `shoal move` carries between machines: what one holds.
use shoal_protocol::transfer::{
    Carrying, ExportedSpace, ImportedPackage, MoveDestination, PackageImport, StoppedExecutions,
};
use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::connection::{Daemon, open_buffered, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    protocol::{self, RemovalMethod, WorkspaceMethod},
    removal::{RemovalResult, UnsavedWork},
    transfer::PackageSource,
};

use shoal_ids::checkpoint_numbers::CheckpointNumber;

/// Package the removal record of the removed space `space` names by ID,
/// which stays, with the records of the spaces its removal took that
/// `carrying` carries, into `file`.
pub async fn export_removed(
    daemon: &impl Daemon,
    space: SpaceSelector,
    file: PathBuf,
    carrying: Carrying,
) -> Result<ExportedSpace> {
    let method = RemovalMethod::ExportRemovedSpace {
        space,
        file,
        carrying,
    };
    request(daemon, method).await
}

/// Record the move of `space` and of the spaces below it that `carrying`
/// carries to `destination`, which may delete the `unsaved` work the caller
/// saw, then stop their agents, saving their sessions, and package them
/// into the file the destination takes, leaving them here until
/// [`end_moved`] ends them or [`abandon_move`] forgets the move; `again`
/// packs again under the move's record, as only its finisher does.
pub async fn export_moving(
    daemon: &impl Daemon,
    space: SpaceSelector,
    unsaved: UnsavedWork,
    destination: MoveDestination,
    carrying: Carrying,
    again: bool,
) -> Result<ExportedSpace> {
    let method = RemovalMethod::ExportMovingSpace {
        space,
        unsaved,
        destination,
        carrying,
        again,
    };
    request(daemon, method).await
}

/// End `space` with the spaces below it that its recorded move carries,
/// once the destination has their package. Returns the removal.
pub async fn end_moved(daemon: &impl Daemon, space: SpaceSelector) -> Result<RemovalResult> {
    request(daemon, RemovalMethod::EndMovedSpace { space }).await
}

/// Forget the recorded move of `space`, which failed, returning the
/// executions whose stopped work its spaces saved once it stopped their
/// agents.
pub async fn abandon_move(daemon: &impl Daemon, space: SpaceSelector) -> Result<StoppedExecutions> {
    request(daemon, RemovalMethod::AbandonMove { space }).await
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

/// [`import`] the package `package` holds as `import` says otherwise:
/// sent on the request's connection once the daemon asks for it.
pub async fn import_streamed(
    daemon: &impl Daemon,
    import: PackageImport,
    package: &mut (impl AsyncRead + Unpin),
) -> Result<ImportedPackage> {
    let import = PackageImport {
        package: PackageSource::Stream,
        ..import
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
