//! Demos: named commands Shoal keeps running in a workspace so the user can
//! try a change, usually from another machine over SSH. A repository names
//! them in `[demos.NAME]`; a preset says how a demo reaches the user, and the
//! repository always supplies the command.
use shoal_protocol::demos::{Progress, Running};

use anyhow::Result;
use tokio::{io::BufReader, net::UnixStream};

use crate::connection::{Daemon, open_buffered, request};
use shoal_protocol::protocol::DemoMethod;
use shoal_protocol::space_names::SpaceSelector;

/// The demos running in `workspace`, or in every workspace.
pub async fn running(
    daemon: &impl Daemon,
    workspace: Option<SpaceSelector>,
) -> Result<Vec<Running>> {
    request(daemon, DemoMethod::ListDemos { workspace }).await
}

/// How far `workspace`'s demo launch `launch` came.
pub async fn progress(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    launch: String,
) -> Result<Progress> {
    request(daemon, DemoMethod::DemoProgress { workspace, launch }).await
}

/// Stop `workspace`'s demo `name`, or all of them.
pub async fn stop(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: Option<String>,
) -> Result<()> {
    request(daemon, DemoMethod::StopDemos { workspace, name }).await
}

/// A stream to a running demo: the demo it opened to, and the connection
/// that carries the demo's bytes both ways from then on, as a TCP
/// connection to a `server` or `web` demo's port does.
pub struct DemoStream {
    pub demo: Running,
    pub connection: BufReader<UnixStream>,
}

/// Open a stream to `workspace`'s running demo `name`, on whichever
/// machine it runs. Fails at once when the daemon refuses, as for a demo
/// outside the caller's scope, one Shoal does not stream, or one on a
/// machine that is offline.
pub async fn open_stream(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: String,
) -> Result<DemoStream> {
    let method = DemoMethod::OpenDemoStream { workspace, name };
    let (connection, body) = open_buffered(daemon, method).await?;
    Ok(DemoStream {
        demo: *Box::<Running>::try_from(body)?,
        connection,
    })
}
