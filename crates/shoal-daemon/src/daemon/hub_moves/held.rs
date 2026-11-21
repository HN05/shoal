//! The package a move holds for the joined machine that started it, as
//! `shoal move -o` does for a space of this machine from another one: that
//! machine reads it over a stream of the hub's links, writes it where its
//! caller named and then ends the space here.
use std::{fs::File, io::Read as _, sync::Arc};

use anyhow::{Context as _, Result, bail};
use shoal_protocol::{
    protocol::{self, Body, Response},
    space_names::SpaceSelector,
};
use tokio::{io::AsyncWriteExt as _, net::UnixStream};

use crate::{
    daemon::{answer::OperationGuard, hub::sending::blocking, manager::Manager},
    transfer::{MoveDestination, MoveLock, MoveRecord, package_name},
};

/// A package a move holds, open, with its size, under the move's lock,
/// which keeps Shoal from finishing the move while it is read.
pub(crate) struct HeldPackage {
    file: File,
    size: u64,
    lock: MoveLock,
}

impl Manager {
    /// The package the recorded move of the space `selector` names holds
    /// here for the joined machine that started it.
    pub(crate) async fn held_package(&self, selector: &SpaceSelector) -> Result<HeldPackage> {
        let record = self.held_move(selector).await?;
        let lock = MoveLock::try_take(&self.paths, &record.space)?
            .context("Shoal is finishing this move, or another reader reads its package")?;
        let path = self.paths.move_packages().join(package_name(&record.space));
        let (file, size) = blocking(move || {
            let file = File::open(&path)?;
            let size = file.metadata()?.len();
            Ok((file, size))
        })
        .await?;
        Ok(HeldPackage { file, size, lock })
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
        let size = Body::HeldPackage(package.size);
        protocol::write(&mut stream, &Response::new(request_id, size)).await?;
        let HeldPackage { mut file, lock, .. } = package;
        loop {
            let (read, chunk) = blocking(move || {
                let mut chunk = vec![0; CHUNK];
                let read = file.read(&mut chunk)?;
                chunk.truncate(read);
                Ok((file, chunk))
            })
            .await?;
            if chunk.is_empty() {
                break;
            }
            file = read;
            stream.write_all(&chunk).await?;
        }
        stream.shutdown().await?;
        lock.release();
        drop(guard);
        Ok(())
    }
}

/// How much of a package one read takes.
const CHUNK: usize = 64 * 1024;
