//! Attaching a terminal on this machine, or a device's, to an agent's
//! session on whichever machine its space runs, which runs Shoal's attach
//! on a terminal of its own for it: what that terminal shows comes back as
//! frames, and the viewer's keys and size go there. A terminal demo's
//! stream carries the same frames.
use anyhow::Result;
use shoal_protocol::{
    attaching::{FromSession, ToSession, Viewer},
    protocol::{Body, WorkspaceMethod},
    space_names::SpaceSelector,
};
use tokio::{
    io::{AsyncWriteExt as _, BufReader, ReadHalf, WriteHalf},
    net::UnixStream,
};

use crate::connection::{Daemon, open_buffered};

type Connection = BufReader<UnixStream>;

/// What the attach's terminal shows, as its machine sends it.
pub struct Screen(ReadHalf<Connection>);

/// Where the viewer's keys and size go.
pub struct Keys(WriteHalf<Connection>);

/// Attach `viewer` to a session of `space` on the machine that has it.
/// Fails at once when the daemon refuses, as for a space's process, or a
/// space on a machine that is offline.
pub async fn attach(
    daemon: &impl Daemon,
    space: SpaceSelector,
    viewer: Viewer,
) -> Result<(Screen, Keys)> {
    let method = WorkspaceMethod::AttachSession { space, viewer };
    let (connection, body) = open_buffered(daemon, method).await?;
    match body.into_result()? {
        Body::Ok => {}
        body => return Err(body.unexpected("Ok")),
    }
    Ok(split(connection))
}

/// The two ways of a connection that carries a terminal's frames.
pub(crate) fn split(connection: Connection) -> (Screen, Keys) {
    let (screen, keys) = tokio::io::split(connection);
    (Screen(screen), Keys(keys))
}

impl Screen {
    /// The next frame; none once the connection ends without the attach's
    /// exit code, as when the link to its machine drops. Reading it is not
    /// cancel-safe: a frame read in part is lost.
    pub async fn next(&mut self) -> Result<Option<FromSession>> {
        FromSession::read(&mut self.0).await
    }
}

impl Keys {
    pub async fn send(&mut self, frame: &ToSession) -> Result<()> {
        self.0.write_all(&frame.encode()).await?;
        Ok(())
    }
}
