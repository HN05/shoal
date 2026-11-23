//! The package a move holds for the joined machine that started it, as
//! `shoal move -o` does for a space of this machine from another one: that
//! machine reads it over a stream of the hub's links, writes it where its
//! caller named and then ends the space here, or gives it back when it
//! could not, and the space stays here. A copy's package, which no move
//! record keeps, waits apart from a move's until its reader reads it, and
//! goes then, or once it waited as long as a move waits for its reader.
use std::{
    fs::File,
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use anyhow::{Context as _, Result, bail};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    protocol::{self, Body, Response},
    space_names::SpaceSelector,
};
use tokio::{io::AsyncWriteExt as _, net::UnixStream};

use crate::{
    daemon::{
        answer::OperationGuard, hub::sending::blocking, log, manager::Manager,
        spaces::HELD_FOR_READER,
    },
    protocol::operations::OperationSpec,
    transfer::{MoveDestination, MoveLock, MovePhase, MoveRecord, held_copies, package_name},
};

/// A package held here for the joined machine that started its move,
/// open, with its size and what keeps it until it is read.
pub(crate) struct HeldPackage {
    file: File,
    size: u64,
    kept: Kept,
}

/// What keeps a held package.
enum Kept {
    /// The recorded move of the space, under the move's lock, which keeps
    /// Shoal from finishing the move while it is read.
    Move { lock: MoveLock, space: SpaceId },
    /// Nothing: a copy's package, whose file its reader took on opening
    /// it, so no other reader takes it too.
    Copy,
}

impl Manager {
    /// The package of the space `selector` names that is held here for the
    /// joined machine that started its move: its recorded move's, or with
    /// `copy` a copy's.
    pub(crate) async fn held_package(
        &self,
        selector: &SpaceSelector,
        copy: bool,
    ) -> Result<HeldPackage> {
        match copy {
            true => self.held_copy(selector).await,
            false => self.held_move_package(selector).await,
        }
    }

    /// The package the recorded move of the space `selector` names holds
    /// here, under the move's lock.
    async fn held_move_package(&self, selector: &SpaceSelector) -> Result<HeldPackage> {
        let record = self.held_move(selector).await?;
        let lock = MoveLock::try_take(&self.paths, &record.space)?
            .context("Shoal is finishing this move, or another reader reads its package")?;
        let path = self.paths.move_packages().join(package_name(&record.space));
        let (file, size) = open_package(path).await?;
        let space = record.space;
        Ok(HeldPackage {
            file,
            size,
            kept: Kept::Move { lock, space },
        })
    }

    /// The package of a copy of the space `selector` names that waits here
    /// for its reader, which it takes: its file goes once open, whether or
    /// not the read ends whole, since its reader cannot read it again.
    async fn held_copy(&self, selector: &SpaceSelector) -> Result<HeldPackage> {
        let space = self.space(selector).await?;
        let path = held_copies(&self.paths.move_packages()).join(package_name(&space.id));
        let (file, size) = (take_package(path).await).with_context(|| {
            format!("no copy of space {selector} waits here for another machine")
        })?;
        Ok(HeldPackage {
            file,
            size,
            kept: Kept::Copy,
        })
    }

    /// Delete the copies' packages that no machine read within
    /// [`HELD_FOR_READER`] of their writing, as when the machine that
    /// started the copy went.
    pub(crate) async fn forget_unread_copies(&self) -> Result<()> {
        let directory = held_copies(&self.paths.move_packages());
        let cutoff = SystemTime::now() - HELD_FOR_READER;
        blocking(move || remove_written_before(&directory, cutoff)).await
    }

    /// Give back the move of the space `selector` names, whose package the
    /// joined machine that started it could not take: nothing of it left
    /// here, so Shoal finishes it as one that packed only, and the space
    /// stays here with its agents' restores queued.
    pub(crate) async fn release_held_move(&self, selector: &SpaceSelector) -> Result<()> {
        let mut record = self.held_move(selector).await?;
        // After a read of it that broke off ends here.
        let lock = MoveLock::take(&self.paths, &record.space).await?;
        record.phase = MovePhase::Packing;
        self.advance_move(&record).await?;
        lock.release();
        let space = record.space.clone();
        let spec = OperationSpec::FinishMove {
            record: Box::new(record),
        };
        self.spawn_operation(space, spec, None, None)
            .await
            .map(drop)
    }

    /// The recorded move of the space `selector` names that holds its
    /// package for another machine.
    async fn held_move(&self, selector: &SpaceSelector) -> Result<MoveRecord> {
        let space = self.space(selector).await?;
        let record = self.move_record(&space.id).await?;
        match record {
            Some(record) if record.destination == MoveDestination::Held => Ok(record),
            _ => bail!("no move of space {selector} holds its package for another machine"),
        }
    }

    /// Answer `request_id` on `stream` with the size of `package`, then
    /// write its bytes there.
    pub(crate) async fn send_held_package(
        self: Arc<Self>,
        mut stream: UnixStream,
        request_id: u64,
        package: HeldPackage,
        guard: OperationGuard,
    ) -> Result<()> {
        let HeldPackage { file, size, kept } = package;
        let sent = send_package(&mut stream, request_id, file, size).await;
        self.package_read(kept, sent.is_ok()).await;
        drop(guard);
        sent
    }

    /// Settle what `kept` a held package once a read of it ended, `whole`
    /// or not: a move waits for its reader to end the space after a whole
    /// read.
    async fn package_read(&self, kept: Kept, whole: bool) {
        match kept {
            Kept::Move { lock, space } if whole => {
                // Its reader writes it where its caller named before it
                // ends the space here.
                if let Err(error) = self.held_package_read(&space).await {
                    log!("the move of {space} waits for its reader no longer: {error:#}");
                }
                lock.release();
            }
            Kept::Move { .. } | Kept::Copy => {}
        }
    }
}

/// Open the package in `path`, with its size.
async fn open_package(path: PathBuf) -> Result<(File, u64)> {
    blocking(move || {
        let file = File::open(&path)?;
        let size = file.metadata()?.len();
        Ok((file, size))
    })
    .await
}

/// Open the package in `path`, with its size, and delete its file, which
/// the open file still reads.
async fn take_package(path: PathBuf) -> Result<(File, u64)> {
    blocking(move || {
        let file = File::open(&path)?;
        let size = file.metadata()?.len();
        std::fs::remove_file(&path)?;
        Ok((file, size))
    })
    .await
}

/// Answer `request_id` on `stream` with `size`, the size of the package in
/// `file`, then write its bytes there.
async fn send_package(
    stream: &mut UnixStream,
    request_id: u64,
    file: File,
    size: u64,
) -> Result<()> {
    let size = Body::HeldPackage(size);
    protocol::write(stream, &Response::new(request_id, size)).await?;
    copy_package(file, stream).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Write what `file` holds to `stream`, reading it off the runtime.
async fn copy_package(mut file: File, stream: &mut UnixStream) -> Result<()> {
    loop {
        let (read, chunk) = blocking(move || {
            let mut chunk = vec![0; CHUNK];
            let read = file.read(&mut chunk)?;
            chunk.truncate(read);
            Ok((file, chunk))
        })
        .await?;
        if chunk.is_empty() {
            return Ok(());
        }
        file = read;
        stream.write_all(&chunk).await?;
    }
}

/// How much of a package one read takes.
const CHUNK: usize = 64 * 1024;

/// Delete each file in `directory` last written before `cutoff`, passing
/// over one its reader took meanwhile.
fn remove_written_before(directory: &Path, cutoff: SystemTime) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        entries => entries?,
    };
    for entry in entries {
        let path = entry?.path();
        let removed = match std::fs::metadata(&path).and_then(|file| file.modified()) {
            Ok(written) if written < cutoff => std::fs::remove_file(&path),
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        };
        match removed {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
