//! `shoal demo` for a `server`, `web` or `simulator` demo another joined
//! machine runs: a loopback port here forwards each connection to the demo
//! through the hub.
use anyhow::{Context as _, Result, bail};
use shoal_protocol::{
    demos::{Kind, PORT, Running},
    space_names::SpaceSelector,
};
use tokio::net::{TcpListener, TcpStream};

use super::presets;
use crate::{
    cli::context::Context,
    client::{connection::CommandPaths, output::demo::Forwarding},
    demos,
};

pub(super) async fn forward(ctx: &Context, demo: Running, machine: &str) -> Result<i32> {
    let selector = SpaceSelector::id(&demo.workspace_id);
    let (demo, port) = forwarded(&ctx.paths, demo, machine).await?;
    let listener = listen(port).await?;
    let local = listener.local_addr()?.port();
    let forwarding = Forwarding {
        name: demo.launch.name.clone(),
        machine: machine.to_owned(),
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

/// Opening a stream to a simulator demo's viewer starts it. Refused for a
/// terminal demo, which serves on no port.
async fn forwarded(paths: &CommandPaths, demo: Running, machine: &str) -> Result<(Running, u16)> {
    let name = demo.launch.name.clone();
    let port = match demo.kind {
        Kind::Server | Kind::Web => demo.launch.port(),
        Kind::Simulator => return presets::start_viewer(paths, &demo).await,
        Kind::Terminal => bail!("demo {name} on machine {machine} is a terminal demo"),
    };
    Ok((
        demo,
        port.with_context(|| format!("demo {name} has no port"))?,
    ))
}

async fn listen(port: u16) -> Result<TcpListener> {
    match TcpListener::bind(("127.0.0.1", port)).await {
        Ok(listener) => Ok(listener),
        Err(_) => (TcpListener::bind(("127.0.0.1", 0)).await)
            .context("listen on a port of this machine's loopback"),
    }
}

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
