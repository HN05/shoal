//! The wrapper's daemon connection. A reader task owns the receive half, so
//! waiting for a control is cancel-safe and adjacent frames are never lost.
use anyhow::{Result, anyhow};
use tokio::{
    io::BufReader,
    net::unix::{OwnedReadHalf, OwnedWriteHalf},
    sync::mpsc,
    task::JoinHandle,
};

use crate::{
    cli::client,
    paths::Paths,
    protocol::{self, Body, Control, ExecutionEvent, Method},
};

pub(super) struct Link {
    writer: OwnedWriteHalf,
    controls: mpsc::Receiver<Result<Control>>,
    reader: JoinHandle<()>,
}

impl Link {
    /// Send `method` and keep the connection for the execution's lifetime.
    pub(super) async fn open(paths: &Paths, method: Method) -> Result<(Self, Body)> {
        let (reader, writer, body) = client::open_split(paths, method).await?;
        Ok((Self::new(reader, writer), body))
    }

    /// Read controls from `reader`, including any already buffered.
    pub(super) fn new(reader: BufReader<OwnedReadHalf>, writer: OwnedWriteHalf) -> Self {
        let (sender, controls) = mpsc::channel(4);
        Self {
            writer,
            controls,
            reader: tokio::spawn(read_controls(reader, sender)),
        }
    }

    pub(super) async fn send(&mut self, event: &ExecutionEvent) -> Result<()> {
        protocol::write(&mut self.writer, event).await
    }

    /// The next control; an error once the daemon disconnects.
    pub(super) async fn recv(&mut self) -> Result<Control> {
        self.controls
            .recv()
            .await
            .unwrap_or_else(|| Err(anyhow!("daemon disconnected")))
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

async fn read_controls(
    mut reader: BufReader<OwnedReadHalf>,
    controls: mpsc::Sender<Result<Control>>,
) {
    loop {
        let control = protocol::read_buffered::<Control>(&mut reader).await;
        let failed = control.is_err();
        if controls.send(control).await.is_err() || failed {
            break;
        }
    }
}
