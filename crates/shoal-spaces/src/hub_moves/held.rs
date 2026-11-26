//! The package a move holds for the joined machine that started it, which
//! that machine reads over a stream of the hub's links. A copy's package,
//! and that of a removed space's record, which no move record keeps, waits
//! apart until its reader reads it, or as long as a move waits for its
//! reader.
use crate::Spaces;
use crate::fsutil::off_runtime;
use std::{
    fs::File,
    io::Read as _,
    path::{Path, PathBuf},
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
    frames::OperationGuard,
    log,
    protocol::operations::OperationSpec,
    spaces::HELD_FOR_READER,
    transfer::{MoveDestination, MoveLock, MovePhase, MoveRecord, held_copies, package_name},
};

/// Open, with its size and what keeps it until it is read.
pub struct HeldPackage {
    file: File,
    size: u64,
    kept: Kept,
}

enum Kept {
    /// The recorded move of the space, under the move's lock, which keeps
    /// Shoal from finishing the move while it is read.
    Move { lock: MoveLock, space: SpaceId },
    /// Nothing: a copy's package, whose file its reader took on opening it,
    /// so no other reader takes it too.
    Copy,
}

impl Spaces {
    /// Its recorded move's, or with `copy` a copy's.
    pub async fn held_package(&self, selector: &SpaceSelector, copy: bool) -> Result<HeldPackage> {
        match copy {
            true => self.held_copy(selector).await,
            false => self.held_move_package(selector).await,
        }
    }

    /// Under the move's lock.
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

    /// Also the package of the space's record once it is removed. Takes the
    /// package: its file goes once open, whether or not the read ends whole,
    /// since its reader cannot read it again.
    async fn held_copy(&self, selector: &SpaceSelector) -> Result<HeldPackage> {
        let id = self.live_or_removed_id(selector).await?;
        let path = held_copies(&self.paths.move_packages()).join(package_name(&id));
        let (file, size) = (take_package(path).await).with_context(|| {
            format!("no copy of space {selector} waits here for another machine")
        })?;
        Ok(HeldPackage {
            file,
            size,
            kept: Kept::Copy,
        })
    }

    /// One this machine has neither of is refused as absent, which the
    /// machine that has it answers for.
    async fn live_or_removed_id(&self, selector: &SpaceSelector) -> Result<SpaceId> {
        let absent = match self.space(selector).await {
            Ok(space) => return Ok(space.id),
            Err(absent) => absent,
        };
        match self.removed_space(selector).await {
            Ok(removed) => Ok(removed.space_id),
            Err(_) => Err(absent),
        }
    }

    /// Those no machine read within [`HELD_FOR_READER`] of their writing, as
    /// when the machine that started the copy went.
    pub async fn forget_unread_copies(&self) -> Result<()> {
        let directory = held_copies(&self.paths.move_packages());
        let cutoff = SystemTime::now() - HELD_FOR_READER;
        off_runtime::run_io(move || remove_written_before(&directory, cutoff)).await
    }

    /// The machine that started it could not take the package. Nothing of it
    /// is left here, so Shoal finishes the move as one that packed only, and
    /// the space stays here with its agents' restores queued.
    pub async fn release_held_move(&self, selector: &SpaceSelector) -> Result<()> {
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
        self.operations
            .spawn(space, spec, None, None)
            .await
            .map(drop)
    }

    async fn held_move(&self, selector: &SpaceSelector) -> Result<MoveRecord> {
        let space = self.space(selector).await?;
        let record = self.move_record(&space.id).await?;
        match record {
            Some(record) if record.destination == MoveDestination::Held => Ok(record),
            _ => bail!("no move of space {selector} holds its package for another machine"),
        }
    }

    /// Answers with the size first, then writes the bytes.
    pub async fn send_held_package(
        &self,
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

    /// A move waits for its reader to end the space after a whole read.
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

async fn open_package(path: PathBuf) -> Result<(File, u64)> {
    off_runtime::run_io(move || {
        let file = File::open(&path)?;
        let size = file.metadata()?.len();
        Ok((file, size))
    })
    .await
}

/// Deletes its file, which the open file still reads.
async fn take_package(path: PathBuf) -> Result<(File, u64)> {
    off_runtime::run_io(move || {
        let file = File::open(&path)?;
        let size = file.metadata()?.len();
        std::fs::remove_file(&path)?;
        Ok((file, size))
    })
    .await
}

/// Answers with `size` first, then writes the bytes.
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

/// Reads off the runtime.
async fn copy_package(mut file: File, stream: &mut UnixStream) -> Result<()> {
    loop {
        let (read, chunk) = off_runtime::run_io(move || {
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

const CHUNK: usize = 64 * 1024;

/// Passes over a file its reader took meanwhile.
pub fn remove_written_before(directory: &Path, cutoff: SystemTime) -> std::io::Result<()> {
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
