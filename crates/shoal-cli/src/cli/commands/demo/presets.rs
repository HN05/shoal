//! What a demo's preset and ports give it: reservations, variables, and how the
//! preset shows the demo.
use std::collections::BTreeMap;

use anyhow::{Context as _, Result};

use super::start::Chosen;
use crate::client::connection::CommandPaths;
use crate::client::leases::{self, Acquisition, PortAcquisition};
use crate::{
    cli::{
        commands::{
            acquisition::{Finish, obtain},
            approvals,
        },
        context::Context,
    },
    config::repo::ConflictPolicy,
    model::{PortRequest, PortReservation, Workspace},
};
use shoal_protocol::demos::{Launch, PORT, Preset, Running, SIM_UDID};
use shoal_protocol::sim::{SimRequest, Simulator};
use shoal_protocol::space_names::SpaceSelector;

/// The workspace's lease `shoal acquire sim` takes by default, so the demo
/// shows the device the agent installed on.
const SIM_LEASE: &str = "default";

#[derive(Debug, Default)]
pub(super) struct Provided {
    pub environment: BTreeMap<String, String>,
}

/// What a request gives, or the exit status of a declined one, which was
/// reported already.
pub(super) type Declinable<T> = std::result::Result<T, i32>;

pub(super) async fn provide(
    ctx: &Context,
    workspace: &Workspace,
    chosen: &Chosen,
    queue: bool,
) -> Result<Declinable<Provided>> {
    let ports = match reserve_ports(ctx, workspace, chosen).await? {
        Ok(ports) => ports,
        Err(declined) => return Ok(Err(declined)),
    };
    Ok(match chosen.demo.preset {
        Some(Preset::Server | Preset::Web) => Ok(server(&ports)),
        Some(Preset::Simulator) => simulator(ctx, workspace, &chosen.name, queue).await?,
        None => Ok(Provided::default()),
    })
}

async fn reserve_ports(
    ctx: &Context,
    workspace: &Workspace,
    chosen: &Chosen,
) -> Result<Declinable<Vec<PortReservation>>> {
    let mut ports = Vec::new();
    for name in port_names(chosen) {
        match reserve_port(ctx, workspace, &name).await? {
            Ok(port) => ports.push(port),
            Err(declined) => return Ok(Err(declined)),
        }
    }
    Ok(Ok(ports))
}

fn server(ports: &[PortReservation]) -> Provided {
    Provided {
        environment: ports
            .first()
            .map(|port| (PORT.to_owned(), port.port.to_string()))
            .into_iter()
            .collect(),
    }
}

/// The workspace's simulator as `SHOAL_SIM_UDID`. The daemon gives the demo
/// the lease when an agent acquired it, so it ends with the demo.
async fn simulator(
    ctx: &Context,
    workspace: &Workspace,
    name: &str,
    queue: bool,
) -> Result<Declinable<Provided>> {
    let sim = match lease_simulator(ctx, workspace, name, queue).await? {
        Ok(sim) => sim,
        Err(declined) => return Ok(Err(declined)),
    };
    let udid = sim.udid.context("the leased simulator has no UDID")?;
    Ok(Ok(Provided {
        environment: BTreeMap::from([(SIM_UDID.to_owned(), udid)]),
    }))
}

fn port_names(chosen: &Chosen) -> Vec<String> {
    match (&chosen.demo.ports, chosen.demo.preset) {
        (Some(ports), _) => ports.clone(),
        (None, Some(preset)) if preset.serves() => vec![chosen.name.clone()],
        (None, _) => Vec::new(),
    }
}

/// Reserve `name`, taking another free port when the configured one is in
/// use, since a demo needs a port, not that number.
async fn reserve_port(
    ctx: &Context,
    workspace: &Workspace,
    name: &str,
) -> Result<Declinable<PortReservation>> {
    let request = PortRequest {
        port: None,
        env_var: None,
        reason: None,
        on_conflict: Some(ConflictPolicy::Auto),
    };
    let acquired = leases::acquire_port(SpaceSelector::id(&workspace.id), name.to_owned(), request)
        .send(&ctx.paths)
        .await?;
    match acquired {
        PortAcquisition::Reserved(reservation) => Ok(Ok(reservation)),
        PortAcquisition::Approval(request) => Ok(Err(approvals::declined(ctx, &request)?)),
        PortAcquisition::Suggested(suggestion) => anyhow::bail!(
            "the daemon suggested port {} instead of reserving a free one",
            suggestion.suggested_port
        ),
    }
}

async fn lease_simulator(
    ctx: &Context,
    workspace: &Workspace,
    name: &str,
    queue: bool,
) -> Result<Declinable<Simulator>> {
    let request = SimRequest {
        reason: Some(format!("Demo {name}")),
        queue,
        ..SimRequest::new(SIM_LEASE.into())
    };
    let outcome = obtain(ctx, "a simulator", queue, async || {
        leases::acquire_simulator(SpaceSelector::id(&workspace.id), request.clone())
            .send(&ctx.paths)
            .await
    })
    .await?;
    match outcome {
        Acquisition::Acquired(sim) => Ok(Ok(sim)),
        declined => Ok(Err(declined.finish(
            ctx,
            |_| Ok(()),
            std::convert::identity,
        )?)),
    }
}

/// Show the demo as its preset does. Over SSH nothing on this machine's
/// screen reaches the user, so the preset says how to reach the demo
/// instead, through a simulator demo's viewer.
pub(super) async fn open(paths: &CommandPaths, preset: Preset, demo: &Running) -> Result<()> {
    let ssh = std::env::var("SSH_CONNECTION").ok();
    let viewer = match preset {
        Preset::Simulator if ssh.is_some() && demo.launch.viewer_command.is_some() => {
            Some(start_viewer(paths, demo).await?.1)
        }
        _ => None,
    };
    for line in instructions(preset, &demo.launch, ssh.as_deref(), viewer)? {
        outln!("{line}");
    }
    if preset == Preset::Simulator && ssh.is_none() {
        show_simulator(&demo.launch)?;
    }
    Ok(())
}

/// Start the viewer of the simulator demo `demo`, wherever it runs, by
/// opening a stream to it: the demo as it is then, and the port the viewer
/// serves on there.
pub(super) async fn start_viewer(paths: &CommandPaths, demo: &Running) -> Result<(Running, u16)> {
    let name = demo.launch.name.clone();
    let space = SpaceSelector::id(&demo.workspace_id);
    let opened = crate::demos::open_stream(paths, space, name.clone()).await?;
    let port = opened.demo.viewer.as_ref().and_then(|viewer| viewer.port);
    let port = port.with_context(|| format!("demo {name} has no viewer"))?;
    Ok((opened.demo, port))
}

fn show_simulator(launch: &Launch) -> Result<()> {
    let udid = launch
        .environment
        .get(SIM_UDID)
        .with_context(|| format!("demo {} has no simulator", launch.name))?;
    std::process::Command::new("open")
        .args(["-a", "Simulator", "--args", "-CurrentDeviceUDID", udid])
        .status()
        .context("open Simulator")?;
    Ok(())
}

fn instructions(
    preset: Preset,
    launch: &Launch,
    ssh: Option<&str>,
    viewer: Option<u16>,
) -> Result<Vec<String>> {
    let host = ssh.and_then(|connection| connection.split_whitespace().nth(2));
    Ok(match preset {
        Preset::Server | Preset::Web => {
            let (Some(port), Some(location)) = (launch.port(), launch.location()) else {
                anyhow::bail!("demo {} has no port", launch.name);
            };
            reach(location, port, host)
        }
        Preset::Simulator => {
            let udid = launch
                .environment
                .get(SIM_UDID)
                .with_context(|| format!("demo {} has no simulator", launch.name))?;
            let url = launch.viewer(viewer).and_then(|viewer| viewer.url);
            match (host, viewer.zip(url)) {
                (Some(host), Some((port, url))) => reach(url, port, Some(host)),
                (Some(_), None) => vec![format!(
                    "Demo {} runs on simulator {udid}, whose viewer is off",
                    launch.name
                )],
                (None, _) => vec![format!("Opening simulator {udid}")],
            }
        }
    })
}

fn reach(location: String, port: u16, host: Option<&str>) -> Vec<String> {
    let mut lines = vec![location];
    if let Some(host) = host {
        let user = std::env::var("USER").unwrap_or_default();
        lines.push(format!(
            "Forward the port first: ssh -L {port}:localhost:{port} {user}@{host}"
        ));
    }
    lines
}

#[cfg(test)]
mod tests;
