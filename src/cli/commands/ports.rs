//! CLI port reservations and interactive conflict suggestions.
use anyhow::{Result, ensure};
use serde_json::json;

use crate::{
    cli::{
        WorkspaceScope,
        client::{self, request},
        context::{Context, optional},
        output::{Palette, Style},
        ui,
    },
    daemon::ports::PortRequest,
    model::{PortCatalog, PortOverview, PortReservation},
    protocol::{Body, Method},
};

use super::workspace_overviews;

/// Acquire a port, offering the daemon's suggestion when the preferred port
/// is taken. Exit 2 means nothing was reserved.
pub(super) async fn acquire(
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
            Body::ApprovalRequest(request) => return super::approvals::declined(ctx, &request),
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
                outln!(
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
            body => return Err(body.unexpected("Port, ApprovalRequest or PortSuggestion")),
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

pub(super) async fn overview(ctx: &Context, scope: WorkspaceScope) -> Result<i32> {
    let Some(workspace) = ui::select_workspace_filter(ctx, scope).await? else {
        return workspace_overviews(
            ctx,
            async |workspace| {
                request::<PortOverview>(
                    &ctx.paths,
                    Method::PortOverview {
                        workspace: workspace.id.clone(),
                    },
                )
                .await
            },
            render_overview,
        )
        .await;
    };
    let overview = request::<PortOverview>(&ctx.paths, Method::PortOverview { workspace }).await?;
    ctx.show(&overview, |overview| {
        render_overview(overview, Palette::stdout(ctx.json))
    })?;
    Ok(0)
}

/// Choose a configured port of the workspace's repository.
pub(super) async fn pick_configured(ctx: &Context, workspace: &str) -> Result<String> {
    let catalog = fetch_catalog(ctx, workspace).await?;
    ensure!(
        !catalog.configured.is_empty(),
        "no configured ports; name the port to reserve"
    );
    let entries = catalog
        .configured
        .into_iter()
        .map(|(name, definition)| {
            let label = match definition.port {
                Some(port) => format!("{name} ({port})"),
                None => name.clone(),
            };
            (name, label)
        })
        .collect();
    ui::pick(ctx, "Port> ", entries)
}

pub(super) fn render_overview(overview: &PortOverview, palette: Palette) {
    for port in &overview.reserved {
        outln!("{}", describe(port, palette));
    }
    if overview.reserved.is_empty() {
        outln!("No reserved ports");
    }
}

pub(super) async fn fetch_catalog(ctx: &Context, workspace: &str) -> Result<PortCatalog> {
    let method = Method::PortCatalog {
        workspace: workspace.to_owned(),
    };
    request::<PortCatalog>(&ctx.paths, method).await
}

pub(super) fn render_catalog(catalog: &PortCatalog, palette: Palette) {
    outln!(
        "Automatic range: {}-{}; conflicts: {}",
        catalog.start,
        catalog.end,
        catalog.on_conflict
    );
    for (name, definition) in &catalog.configured {
        outln!(
            "{}: {}{} (conflicts: {}){}",
            palette.paint(Style::Heading, name),
            definition
                .port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "automatic".into()),
            optional(definition.env.as_deref(), |env| format!(" {env}")),
            definition.on_conflict.unwrap_or(catalog.on_conflict),
            if definition.requires_approval {
                format!("; approval required ({})", definition.approval_lifetime)
            } else {
                String::new()
            }
        );
    }
}
