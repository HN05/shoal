//! CLI port reservations and interactive conflict suggestions.
use anyhow::{Result, bail};
use serde_json::json;

use crate::{
    cli::PortCommand,
    client::{self, request},
    context::{Context, optional},
    model::{PortOverview, PortReservation},
    output::{Palette, Style},
    ports::PortRequest,
    protocol::{Body, Method},
    ui::{self, Fallback},
};

pub(super) async fn run(ctx: &Context, command: PortCommand) -> Result<i32> {
    match command {
        PortCommand::Reserve {
            name,
            workspace,
            port,
            env,
            reason,
            on_conflict,
        } => {
            let workspace =
                ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
            let request = PortRequest {
                port,
                env_var: env,
                reason,
                on_conflict,
            };
            reserve(ctx, workspace, name, request).await
        }
        PortCommand::List { workspace, all } => {
            let workspace = ui::select_workspace_filter(ctx, workspace, all).await?;
            let ports = request!(&ctx.paths, Method::ListPorts { workspace }, Ports);
            // Owners are shown by name; JSON output keeps the IDs.
            let workspaces = if ctx.json {
                vec![]
            } else {
                client::workspaces(&ctx.paths).await?
            };
            ctx.show(&ports, |ports| {
                let palette = Palette::stdout(ctx.json);
                for port in ports {
                    let owner = workspaces
                        .iter()
                        .find(|w| w.id == port.workspace_id)
                        .map(|w| w.name.as_str())
                        .unwrap_or(&port.workspace_id);
                    println!("{owner}/{}", describe(port, palette));
                }
            })?;
            Ok(0)
        }
        PortCommand::Release { name, workspace } => {
            let workspace =
                ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
            client::call(&ctx.paths, Method::ReleasePort { workspace, name }).await?;
            ctx.emit_styled(
                Style::Success,
                "Port reservation released",
                json!({"released": true}),
            )?;
            Ok(0)
        }
    }
}

/// Reserve a port, offering the daemon's suggestion when the preferred port
/// is taken. Exit 2 means nothing was reserved.
async fn reserve(
    ctx: &Context,
    workspace: String,
    name: String,
    mut request: PortRequest,
) -> Result<i32> {
    loop {
        let method = Method::ReservePort {
            workspace: workspace.clone(),
            name: name.clone(),
            request: request.clone(),
        };
        match client::call(&ctx.paths, method).await? {
            Body::Port(reservation) => {
                ctx.emit(
                    &describe(&reservation, Palette::stdout(ctx.json)),
                    &reservation,
                )?;
                return Ok(0);
            }
            Body::PortSuggestion(proposal) => {
                if !ctx.interactive() {
                    let mut value = serde_json::to_value(&proposal)?;
                    value["reserved"] = json!(false);
                    ctx.emit_styled(
                        Style::Warning,
                        &format!(
                            "{}: port {} unavailable; suggested {}. Accept with --port {}",
                            proposal.name,
                            proposal.requested_port,
                            proposal.suggested_port,
                            proposal.suggested_port
                        ),
                        value,
                    )?;
                    return Ok(super::EXIT_BUSY);
                }
                println!(
                    "{}: port {} {}; suggested {}",
                    proposal.name,
                    proposal.requested_port,
                    Palette::stdout(ctx.json).paint(Style::Warning, "unavailable"),
                    proposal.suggested_port
                );
                let accepted = ui::confirm(
                    ctx,
                    &format!("Reserve suggested port {}?", proposal.suggested_port),
                    &format!("--port {}", proposal.suggested_port),
                )?;
                if !accepted {
                    return Ok(super::EXIT_BUSY);
                }
                request = PortRequest {
                    port: Some(proposal.suggested_port),
                    env_var: Some(proposal.env_var),
                    reason: proposal.reason,
                    on_conflict: request.on_conflict,
                };
            }
            _ => bail!("unexpected port response"),
        }
    }
}

fn describe(port: &PortReservation, palette: Palette) -> String {
    format!(
        "{}={} ({}){}",
        palette.paint(Style::Heading, &port.name),
        port.port,
        port.env_var,
        optional(port.reason.as_deref(), |r| format!("  {r}"))
    )
}

pub(super) async fn overview(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let overview = request!(&ctx.paths, Method::PortOverview { workspace }, PortOverview);
    ctx.show(&overview, |overview| {
        render_overview(overview, Palette::stdout(ctx.json))
    })?;
    Ok(0)
}

fn render_overview(overview: &PortOverview, palette: Palette) {
    for port in &overview.reserved {
        println!("{}", describe(port, palette));
    }
    for (name, definition) in &overview.configured {
        if overview.reserved.iter().any(|p| &p.name == name) {
            continue;
        }
        println!(
            "{name}: not reserved (preferred: {}; conflicts: {:?})",
            definition
                .port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "automatic".into()),
            definition.on_conflict.unwrap_or(overview.on_conflict)
        );
    }
    if overview.reserved.is_empty() && overview.configured.is_empty() {
        println!("No configured or reserved ports");
    }
}
