//! Requests about a workspace's demos, and the streams to a running one.
use shoal_protocol::attaching::Terminal;
use shoal_protocol::demos::{Kind, Progress, Running};

use anyhow::{Result, bail};
use tokio::{io::BufReader, net::UnixStream};

use crate::attaching::{Keys, Screen, split};
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
/// outside the caller's scope, one that takes no connection, such as a
/// simulator demo whose viewer is off, or one on a machine that is
/// offline.
pub async fn open_stream(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: String,
) -> Result<DemoStream> {
    open(daemon, workspace, name, Terminal::default()).await
}

/// [`open_stream`], showing a terminal demo on a terminal made like
/// `terminal` there.
async fn open(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: String,
    terminal: Terminal,
) -> Result<DemoStream> {
    let method = DemoMethod::OpenDemoStream {
        workspace,
        name,
        terminal,
    };
    let (connection, body) = open_buffered(daemon, method).await?;
    Ok(DemoStream {
        demo: *Box::<Running>::try_from(body)?,
        connection,
    })
}

/// A stream to a running terminal demo: the demo, what its terminal shows
/// and where the viewer's keys and size go.
pub struct TerminalStream {
    pub demo: Running,
    pub screen: Screen,
    pub keys: Keys,
}

/// [`open_stream`] for a terminal demo, shown on a terminal made like
/// `terminal` on its machine, whose frames it reads and writes; fails for
/// a demo of another kind.
pub async fn open_terminal(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: String,
    terminal: Terminal,
) -> Result<TerminalStream> {
    let DemoStream { demo, connection } = open(daemon, workspace, name, terminal).await?;
    if demo.kind != Kind::Terminal {
        bail!(
            "demo {} is a {} demo, not a terminal",
            demo.launch.name,
            demo.kind
        );
    }
    let (screen, keys) = split(connection);
    Ok(TerminalStream { demo, screen, keys })
}
