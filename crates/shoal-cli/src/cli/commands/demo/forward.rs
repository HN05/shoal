//! `shoal demo [name] --workspace S` for a `server`, `web` or `simulator`
//! demo another joined machine runs: a port on this machine's loopback, the
//! demo's own number, or its viewer's, when it is free, whose every
//! connection goes to the demo through the hub, until Ctrl-C.
use anyhow::{Context as _, Result, bail};
use shoal_protocol::{
    demos::{Kind, PORT, Running},
    hub::directory::{HubView, Listed},
    space_names::SpaceSelector,
};
use tokio::net::{TcpListener, TcpStream};

use super::{lookup::find, open::pick, presets};
use crate::{
    cli::context::Context,
    client::{connection::CommandPaths, output::demo::Forwarding},
    demos,
};

pub(super) async fn forward(
    ctx: &Context,
    name: Option<String>,
    (space, view): (Listed, HubView),
) -> Result<i32> {
    let machine = view.directory.machine(&space.machine);
    let machine = machine.map_or_else(|| space.machine.to_string(), |machine| machine.name.clone());
    let selector = SpaceSelector::id(&space.space.id);
    let running = demos::running(&ctx.paths, Some(selector.clone())).await?;
    let demo = match name {
        Some(name) => find(running, &name, None)?,
        None => pick(ctx, running)?,
    };
    let (demo, port) = forwarded(&ctx.paths, demo, &machine).await?;
    let listener = listen(port).await?;
    let local = listener.local_addr()?.port();
    let forwarding = Forwarding {
        name: demo.launch.name.clone(),
        machine,
        location: local_location(&demo, local)?,
        port: local,
    };
    ctx.show(&forwarding, |forwarding| {
        outln!("{}", forwarding.location);
        eprintln!(
            "Forwarding demo {} of machine {}; Ctrl-C stops",
            forwarding.name, forwarding.machine
        );
    })?;
    tokio::select! {
        served = serve(ctx, &listener, &selector, &demo.launch.name) => served?,
        signal = tokio::signal::ctrl_c() => signal?,
    }
    Ok(0)
}

/// The port `demo`, which `machine` runs, serves on there, with the demo
/// as it is then: a simulator demo's viewer's, which opening a stream to it
/// starts. Refused for a demo of a kind that opens only there.
async fn forwarded(paths: &CommandPaths, demo: Running, machine: &str) -> Result<(Running, u16)> {
    let name = demo.launch.name.clone();
    let port = match demo.kind {
        Kind::Server | Kind::Web => demo.launch.port(),
        Kind::Simulator => return presets::start_viewer(paths, &demo).await,
        Kind::Terminal => {
            bail!("demo {name} is a terminal demo on machine {machine}; open it there")
        }
    };
    Ok((
        demo,
        port.with_context(|| format!("demo {name} has no port"))?,
    ))
}

/// A listener on loopback: on `port` when it is free, else on any port.
async fn listen(port: u16) -> Result<TcpListener> {
    match TcpListener::bind(("127.0.0.1", port)).await {
        Ok(listener) => Ok(listener),
        Err(_) => (TcpListener::bind(("127.0.0.1", 0)).await)
            .context("listen on a port of this machine's loopback"),
    }
}

/// Where the user reaches `demo` on this machine's port `local`: a web
/// demo's URL or a simulator demo's viewer's, otherwise its address.
fn local_location(demo: &Running, local: u16) -> Result<String> {
    if demo.kind == Kind::Simulator {
        let viewer = demo.launch.viewer(Some(local));
        return viewer
            .and_then(|viewer| viewer.url)
            .context("the demo has no viewer");
    }
    let mut launch = demo.launch.clone();
    launch
        .environment
        .insert(PORT.to_owned(), local.to_string());
    launch.location().context("the demo has no address")
}

/// Send each connection `listener` takes to `space`'s demo `name`, until
/// the listener fails.
async fn serve(
    ctx: &Context,
    listener: &TcpListener,
    space: &SpaceSelector,
    name: &str,
) -> Result<()> {
    loop {
        let (connection, _) = listener.accept().await?;
        let (paths, space, name) = (ctx.paths.clone(), space.clone(), name.to_owned());
        tokio::spawn(async move {
            if let Err(error) = relay(&paths, connection, space, &name).await {
                eprintln!("warning: a connection to demo {name} failed: {error:#}");
            }
        });
    }
}

/// Carry `connection`'s bytes to and from `space`'s demo `name`, both
/// ways, until both sides end.
async fn relay(
    paths: &CommandPaths,
    mut connection: TcpStream,
    space: SpaceSelector,
    name: &str,
) -> Result<()> {
    let mut stream = demos::open_stream(paths, space, name.to_owned()).await?;
    tokio::io::copy_bidirectional(&mut connection, &mut stream.connection).await?;
    Ok(())
}
