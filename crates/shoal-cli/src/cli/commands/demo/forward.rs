//! `shoal demo [name] --workspace S` for a `server` or `web` demo another
//! joined machine runs: a port on this machine's loopback, the demo's own
//! number when it is free, whose every connection goes to the demo through
//! the hub, until Ctrl-C.
use anyhow::{Context as _, Result, ensure};
use shoal_protocol::{
    demos::{Kind, PORT, Running},
    hub::directory::{HubView, Listed},
    space_names::SpaceSelector,
};
use tokio::net::{TcpListener, TcpStream};

use super::{lookup::find, open::pick};
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
    let port = forwarded_port(&demo, &machine)?;
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

/// The port `demo`, which `machine` runs, serves on there; refused for a
/// demo of a kind that opens only there.
fn forwarded_port(demo: &Running, machine: &str) -> Result<u16> {
    let name = &demo.launch.name;
    ensure!(
        matches!(demo.kind, Kind::Server | Kind::Web),
        "demo {name} is a {} demo on machine {machine}; open it there",
        demo.kind
    );
    demo.launch
        .port()
        .with_context(|| format!("demo {name} has no port"))
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
/// demo's URL, otherwise its address.
fn local_location(demo: &Running, local: u16) -> Result<String> {
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
