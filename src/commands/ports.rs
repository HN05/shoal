//! CLI port reservations and interactive conflict suggestions.
use anyhow::Result;
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

use super::WorkspaceOverviewResult;

pub(super) async fn run(
    ctx: &Context,
    command: Option<PortCommand>,
    workspace: Option<String>,
    all: bool,
) -> Result<i32> {
    match command {
        Some(PortCommand::Acquire {
            name,
            workspace,
            port,
            env,
            reason,
            on_conflict,
        }) => {
            let workspace =
                ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
            let request = PortRequest {
                port,
                env_var: env,
                reason,
                on_conflict,
            };
            acquire(ctx, workspace, name, request).await
        }
        Some(PortCommand::List { workspace, all }) => overview(ctx, workspace, all).await,
        Some(PortCommand::Release { name, workspace }) => {
            let workspace =
                ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
            client::request::<()>(&ctx.paths, Method::PortRelease { workspace, name }).await?;
            ctx.emit_styled(
                Style::Success,
                "Port reservation released",
                json!({"released": true}),
            )?;
            Ok(0)
        }
        None => overview(ctx, workspace, all).await,
    }
}

/// Acquire a port, offering the daemon's suggestion when the preferred port
/// is taken. Exit 2 means nothing was reserved.
async fn acquire(
    ctx: &Context,
    workspace: String,
    name: String,
    mut request: PortRequest,
) -> Result<i32> {
    loop {
        let method = Method::PortAcquire {
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
            Body::AccessRequest(request) => return super::access::declined(ctx, &request),
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
                    &format!("Acquire suggested port {}?", proposal.suggested_port),
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
            body => return Err(body.unexpected("Port, AccessRequest or PortSuggestion")),
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

async fn overview(ctx: &Context, workspace: Option<String>, all: bool) -> Result<i32> {
    if all {
        let mut overviews = Vec::new();
        for workspace in client::workspaces(&ctx.paths).await? {
            let method = Method::PortOverview {
                workspace: workspace.id.clone(),
            };
            overviews.push(match request::<PortOverview>(&ctx.paths, method).await {
                Ok(overview) => WorkspaceOverviewResult::Ready(overview),
                Err(error) => WorkspaceOverviewResult::failed(workspace, format!("{error:#}")),
            });
        }
        let failed = overviews.iter().any(WorkspaceOverviewResult::is_failed);
        ctx.show(&overviews, |overviews| {
            let palette = Palette::stdout(ctx.json);
            for overview in overviews {
                match overview {
                    WorkspaceOverviewResult::Ready(overview) => {
                        println!(
                            "{}",
                            palette.paint(Style::Heading, &overview.workspace.name)
                        );
                        render_overview(overview, palette);
                    }
                    WorkspaceOverviewResult::Failed { workspace, error } => println!(
                        "{}: {}",
                        palette.paint(Style::Heading, &workspace.name),
                        palette.paint(Style::Warning, error)
                    ),
                }
            }
        })?;
        return Ok(i32::from(failed));
    } else {
        let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
        let overview =
            request::<PortOverview>(&ctx.paths, Method::PortOverview { workspace }).await?;
        ctx.show(&overview, |overview| {
            render_overview(overview, Palette::stdout(ctx.json))
        })?;
    }
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
            "{name}: not reserved (preferred: {}; conflicts: {})",
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
