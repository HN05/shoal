//! What a demo's preset and ports give it: reservations made before it
//! starts, the variables its command and `open` receive, and how the preset
//! shows it to the user.
use std::collections::BTreeMap;

use anyhow::{Context as _, Result};

use super::Chosen;
use crate::{
    cli::{
        client,
        commands::{
            acquisition::{Acquisition, obtain},
            approvals,
        },
        context::Context,
    },
    config::repo::ConflictPolicy,
    daemon::ports::PortRequest,
    demos::{Launch, Preset},
    model::{PortReservation, Workspace},
    protocol::{Body, Method},
    sim::{SimRequest, Simulator},
};

/// The variable a web demo's port reaches its command as.
const PORT: &str = "PORT";
/// The variable a simulator demo's device reaches its command as.
const SIM_UDID: &str = "SHOAL_SIM_UDID";
/// The workspace's lease `shoal acquire sim` takes by default, so the demo
/// shows the device the agent installed on.
const SIM_LEASE: &str = "default";

/// What the preset and ports provide the demo's command.
#[derive(Debug, Default)]
pub(super) struct Provided {
    pub environment: BTreeMap<String, String>,
}

/// What a request gives, or the exit status of a declined one, which was
/// reported already.
pub(super) type Declinable<T> = std::result::Result<T, i32>;

/// Reserve the demo's ports, then set up what its preset provides.
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
        Some(Preset::Web) => Ok(web(&ports)),
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

/// A web demo's first port, as `PORT`.
fn web(ports: &[PortReservation]) -> Provided {
    Provided {
        environment: ports
            .first()
            .map(|port| (PORT.to_owned(), port.port.to_string()))
            .into_iter()
            .collect(),
    }
}

/// The workspace's simulator as `SHOAL_SIM_UDID`. The lease stays with the
/// workspace, as one `shoal acquire sim` takes does.
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

/// The configured ports, or for a web demo one named after it.
fn port_names(chosen: &Chosen) -> Vec<String> {
    match (&chosen.demo.ports, chosen.demo.preset) {
        (Some(ports), _) => ports.clone(),
        (None, Some(Preset::Web)) => vec![chosen.name.clone()],
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
    let method = Method::PortAcquire {
        workspace: workspace.id.clone(),
        name: name.to_owned(),
        request: PortRequest {
            port: None,
            env_var: None,
            reason: None,
            on_conflict: Some(ConflictPolicy::Auto),
        },
    };
    match client::call(&ctx.paths, method).await? {
        Body::Port(reservation) => Ok(Ok(reservation)),
        Body::ApprovalRequest(request) => Ok(Err(approvals::declined(ctx, &request)?)),
        body => Err(body.unexpected("Port or ApprovalRequest")),
    }
}

async fn lease_simulator(
    ctx: &Context,
    workspace: &Workspace,
    name: &str,
    queue: bool,
) -> Result<Declinable<Simulator>> {
    let request = SimRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        clean: false,
        name: SIM_LEASE.into(),
        profile: None,
        device: None,
        runtime: None,
        reason: Some(format!("Demo {name}")),
        queue,
    };
    let outcome = obtain(ctx, "a simulator", queue, async || {
        let method = Method::SimAcquire {
            workspace: workspace.id.clone(),
            request: request.clone(),
        };
        client::request::<Acquisition<Simulator>>(&ctx.paths, method).await
    })
    .await?;
    match outcome {
        Acquisition::Acquired(sim) => Ok(Ok(sim)),
        declined => Ok(Err(declined.finish(
            ctx,
            |_| Ok(()),
            |message| serde_json::json!({"acquired": false, "code": "simulator_busy", "message": message}),
        )?)),
    }
}

/// Show the demo as its preset does. Over SSH nothing on this machine's
/// screen reaches the user, so the preset says how to reach the demo instead.
pub(super) fn open(preset: Preset, launch: &Launch) -> Result<()> {
    let ssh = std::env::var("SSH_CONNECTION").ok();
    for line in instructions(preset, launch, ssh.as_deref())? {
        outln!("{line}");
    }
    if preset == Preset::Simulator && ssh.is_none() {
        let udid = launch
            .environment
            .get(SIM_UDID)
            .with_context(|| format!("demo {} has no simulator", launch.name))?;
        std::process::Command::new("open")
            .args(["-a", "Simulator", "--args", "-CurrentDeviceUDID", udid])
            .status()
            .context("open Simulator")?;
    }
    Ok(())
}

/// What opening tells the user, given the `SSH_CONNECTION` the session has.
fn instructions(preset: Preset, launch: &Launch, ssh: Option<&str>) -> Result<Vec<String>> {
    let host = ssh.and_then(|connection| connection.split_whitespace().nth(2));
    Ok(match preset {
        Preset::Web => {
            let port = launch
                .environment
                .get(PORT)
                .with_context(|| format!("demo {} has no port", launch.name))?;
            let mut lines = vec![format!("http://localhost:{port}")];
            if let Some(host) = host {
                let user = std::env::var("USER").unwrap_or_default();
                lines.push(format!(
                    "Forward the port first: ssh -L {port}:localhost:{port} {user}@{host}"
                ));
            }
            lines
        }
        Preset::Simulator => {
            let udid = launch
                .environment
                .get(SIM_UDID)
                .with_context(|| format!("demo {} has no simulator", launch.name))?;
            match host {
                Some(host) => vec![format!(
                    "Demo {} runs on simulator {udid}; view it with Screen Sharing to {host}",
                    launch.name
                )],
                None => vec![format!("Opening simulator {udid}")],
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(preset: Preset, variable: &str, value: &str) -> Launch {
        Launch {
            name: "app".into(),
            preset: Some(preset),
            open: None,
            environment: BTreeMap::from([(variable.to_owned(), value.to_owned())]),
            host: "zmx".into(),
            place: "first-app".into(),
        }
    }

    #[test]
    fn web_demos_print_their_url_and_over_ssh_how_to_forward_it() {
        let web = launch(Preset::Web, PORT, "49200");
        assert_eq!(
            instructions(Preset::Web, &web, None).unwrap(),
            ["http://localhost:49200"]
        );
        let over_ssh = instructions(Preset::Web, &web, Some("10.0.0.2 52000 10.0.0.1 22")).unwrap();
        assert_eq!(over_ssh[0], "http://localhost:49200");
        assert!(
            over_ssh[1].starts_with("Forward the port first: ssh -L 49200:localhost:49200 ")
                && over_ssh[1].ends_with("@10.0.0.1"),
            "{}",
            over_ssh[1]
        );
    }

    #[test]
    fn simulator_demos_name_their_device_over_ssh() {
        let app = launch(Preset::Simulator, SIM_UDID, "UDID-1");
        assert_eq!(
            instructions(Preset::Simulator, &app, Some("10.0.0.2 52000 10.0.0.1 22")).unwrap(),
            ["Demo app runs on simulator UDID-1; view it with Screen Sharing to 10.0.0.1"]
        );
        assert_eq!(
            instructions(Preset::Simulator, &app, None).unwrap(),
            ["Opening simulator UDID-1"]
        );
    }

    #[test]
    fn web_demos_reserve_a_port_named_after_them_unless_ports_are_configured() {
        let chosen = |preset, ports: Option<Vec<String>>| Chosen {
            name: "site".into(),
            demo: crate::demos::Demo {
                cmd: vec!["serve".into()],
                preset,
                open: None,
                ports,
            },
            command: Vec::new(),
        };
        assert_eq!(port_names(&chosen(Some(Preset::Web), None)), ["site"]);
        assert_eq!(
            port_names(&chosen(Some(Preset::Web), Some(vec!["web".into()]))),
            ["web"]
        );
        assert!(port_names(&chosen(None, None)).is_empty());
    }
}
