//! The package a move holds for the joined machine that started it, as
//! `shoal move -o` does for a space of this machine from another one: that
//! machine reads it over a stream of the hub's links, writes it where its
//! caller named and then ends the space here.
use std::{fs::File, io::Read as _, sync::Arc};

use anyhow::{Result, ensure};
use shoal_protocol::{
    protocol::{self, Body, Response},
    space_names::SpaceSelector,
};
use tokio::{io::AsyncWriteExt as _, net::UnixStream};

use crate::{
    daemon::{answer::OperationGuard, hub::sending::blocking, manager::Manager},
    transfer::{MoveDestination, package_name},
};

/// A package a move holds, open, with its size.
pub(crate) struct HeldPackage {
    file: File,
    size: u64,
}

impl Manager {
    /// The package the recorded move of the space `selector` names holds
    /// here for the joined machine that started it.
    pub(crate) async fn held_package(&self, selector: &SpaceSelector) -> Result<HeldPackage> {
        let space = self.space(selector).await?;
        let record = self.move_record(&space.id).await?;
        ensure!(
            record.is_some_and(|record| record.destination == MoveDestination::Held),
            "no move of space {selector} holds its package for another machine"
        );
        let path = self.paths.move_packages().join(package_name(&space.id));
        blocking(move || {
            let file = File::open(&path)?;
            let size = file.metadata()?.len();
            Ok(HeldPackage { file, size })
        })
        .await
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
        let mut file = package.file;
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
        drop(guard);
        Ok(())
    }
}

/// How much of a package one read takes.
const CHUNK: usize = 64 * 1024;
