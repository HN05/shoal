//! An attached file's bytes streamed to a caller, as to one on another
//! machine.
use crate::Spaces;

use anyhow::{Context as _, Result};
use shoal_protocol::{files::FileBytes, protocol::Body, space_names::SpaceSelector};
use std::{fs::File, io::Read as _};
use tokio::{net::UnixStream, sync::mpsc};

use crate::frames::{self, OperationGuard};

pub struct OpenedCopy(File);

impl Spaces {
    /// Also opens a file kept for a removed space of that ID.
    pub async fn open_attached_file(
        &self,
        selector: &SpaceSelector,
        id: i64,
    ) -> Result<OpenedCopy> {
        let files = self.attached_files(selector).await?;
        let file = (files.iter().find(|file| file.id == id))
            .with_context(|| format!("space {selector} has no attached file {id}"))?;
        // Every record of this machine's store names its copy here.
        let path = file
            .path()
            .context("the attached file's copy is elsewhere")?;
        let copy = File::open(path).with_context(|| format!("read {}", path.display()))?;
        Ok(OpenedCopy(copy))
    }

    /// Sends the end last. Stops when the client hangs up or a daemon restart
    /// closes the answer.
    pub async fn stream_attached_file(
        &self,
        stream: UnixStream,
        id: u64,
        OpenedCopy(copy): OpenedCopy,
        startup: OperationGuard,
    ) -> Result<()> {
        let (mut reader, writer) = stream.into_split();
        let mut client = frames::Frames::new(&self.gates, writer, id);
        drop(startup);
        let mut frames = read_frames(copy);
        loop {
            let frame = tokio::select! {
                frame = frames.recv() => frame.context("the attached file's reader ended")??,
                () = frames::hangup(&mut reader) => return Ok(()),
            };
            let end = frame == FileBytes::End;
            if !client.send(Body::FileBytes(frame)).await? || end {
                return Ok(());
            }
        }
    }
}

/// Reads on the blocking pool one frame ahead of the reader, until the
/// reader drops them.
fn read_frames(mut copy: File) -> mpsc::Receiver<Result<FileBytes>> {
    let (sender, frames) = mpsc::channel(1);
    tokio::task::spawn_blocking(move || {
        let mut buffer = vec![0; frames::CHUNK];
        loop {
            let frame = match copy.read(&mut buffer) {
                Ok(0) => Ok(FileBytes::End),
                Ok(read) => Ok(FileBytes::data(&buffer[..read])),
                Err(error) => Err(anyhow::Error::from(error).context("read an attached file")),
            };
            let last = !matches!(frame, Ok(FileBytes::Data(_)));
            if sender.blocking_send(frame).is_err() || last {
                return;
            }
        }
    });
    frames
}
