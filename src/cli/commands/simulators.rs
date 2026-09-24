//! CLI simulator requests, retry policy, and audit rendering.
use anyhow::Result;
use serde_json::json;

use super::{Attempt, EXIT_BUSY, retry_while_busy};
use crate::{
    cli::{
        SimCommand,
        client::{self, request},
        context::{Context, optional},
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    protocol::{Body, Method},
    sim::{SimRequest, Simulator, SimulatorCatalog, SimulatorOverview, audit::AuditEntry},
};

pub(super) async fn run(
    ctx: &Context,
    command: Option<SimCommand>,
    workspace: Option<String>,
    all: bool,
) -> Result<i32> {
    match command {
        Some(SimCommand::Catalog) => {
            let catalog = request::<SimulatorCatalog>(&ctx.paths, Method::SimCatalog).await?;
            ctx.show(&catalog, |catalog| {
                println!(
                    "{}",
                    serde_json::to_string_pretty(catalog).unwrap_or_default()
                );
            })?;
            Ok(0)
        }
        Some(SimCommand::List { workspace, all }) => overview(ctx, workspace, all).await,
        Some(SimCommand::Acquire {
            workspace,
            name,
            profile,
            device,
            runtime,
            reason,
            clean,
            wait,
        }) => {
            let workspace =
                ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
            let request = SimRequest {
                request_id: uuid::Uuid::new_v4().to_string(),
                clean,
                name,
                profile,
                device,
                runtime,
                reason,
            };
            let mut approval = None;
            let outcome = retry_while_busy(wait, async || {
                let method = Method::SimAcquire {
                    workspace: workspace.clone(),
                    request: request.clone(),
                };
                approval = None;
                Ok(match client::call(&ctx.paths, method).await? {
                    Body::Simulator(sim) => Attempt::Ready(Ok(sim)),
                    Body::AccessRequest(request) => super::access::attempt(&mut approval, request),
                    Body::Busy { message } => Attempt::Busy(message),
                    body => return Err(body.unexpected("Simulator, AccessRequest or Busy")),
                })
            })
            .await?;
            match outcome {
                Ok(Ok(sim)) => {
                    ctx.emit_styled(Style::Success, &describe(&sim), &sim)?;
                    Ok(0)
                }
                Ok(Err(request)) => super::access::declined(ctx, &request),
                Err(_) if approval.is_some() => super::access::declined(ctx, &approval.unwrap()),
                Err(message) => {
                    ctx.emit_styled(
                        Style::Warning,
                        &message,
                        json!({"acquired": false, "code": "simulator_busy", "message": message}),
                    )?;
                    Ok(EXIT_BUSY)
                }
            }
        }
        Some(SimCommand::History {
            workspace,
            all,
            limit,
            before,
        }) => {
            let workspace = ui::select_workspace_filter(ctx, workspace, all).await?;
            let entries = request::<Vec<AuditEntry>>(
                &ctx.paths,
                Method::SimHistory {
                    workspace,
                    limit,
                    before,
                },
            )
            .await?;
            ctx.show(&entries, |entries| {
                for entry in entries {
                    render_history_entry(entry);
                }
                if entries.is_empty() {
                    println!("No clean-device requests");
                }
            })?;
            Ok(0)
        }
        Some(SimCommand::Release { name, workspace }) => {
            let workspace =
                ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
            client::request::<()>(&ctx.paths, Method::SimRelease { workspace, name }).await?;
            ctx.emit_styled(
                Style::Success,
                "Simulator released",
                json!({"released": true}),
            )?;
            Ok(0)
        }
        None => overview(ctx, workspace, all).await,
    }
}

async fn overview(ctx: &Context, workspace: Option<String>, all: bool) -> Result<i32> {
    let workspace = ui::select_workspace_filter(ctx, workspace, all).await?;
    let overview =
        request::<SimulatorOverview>(&ctx.paths, Method::SimOverview { workspace }).await?;
    ctx.show(&overview, |overview| {
        render_overview(overview, Palette::stdout(ctx.json))
    })?;
    Ok(0)
}

fn render_overview(overview: &SimulatorOverview, palette: Palette) {
    println!(
        "Capacity: {} booted, {} devices",
        overview.policy.max_booted, overview.policy.max_devices
    );
    for (name, profile) in &overview.policy.profiles {
        let preferred = if overview.preferred.contains(name) {
            " (preferred)"
        } else if overview.policy.default.as_deref() == Some(name) {
            " (default)"
        } else {
            ""
        };
        println!(
            "{}: {} / {}{}",
            palette.paint(Style::Heading, name),
            profile.device,
            profile.runtime,
            preferred
        );
    }
    for sim in &overview.simulators {
        println!(
            "{}  {}  {}  {}  {}",
            sim.udid.as_deref().unwrap_or("pending"),
            sim.lease_name.as_deref().unwrap_or("idle"),
            palette.simulator_state(sim.state),
            sim.device,
            sim.runtime
        );
    }
    if overview.policy.profiles.is_empty() && overview.simulators.is_empty() {
        println!("No configured profiles or managed simulators");
    }
}

fn describe(sim: &Simulator) -> String {
    format!(
        "{}={} ({}, {})",
        sim.lease_name.as_deref().unwrap_or("default"),
        sim.udid.as_deref().unwrap_or("pending"),
        sim.device,
        sim.runtime
    )
}

fn render_history_entry(entry: &AuditEntry) {
    let request = &entry.request;
    println!(
        "#{} at {}  {}/{}  {}  action={}  erased-apps={}  actor={}\n  reason: {}{}",
        entry.id,
        request.requested_at,
        request.workspace_name,
        request.request.name,
        request.status,
        request
            .action
            .map(|a| a.to_string())
            .unwrap_or_else(|| "none".into()),
        request
            .apps_removed
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unknown/not erased".into()),
        request.execution_id.as_deref().unwrap_or("unscoped caller"),
        request.request.reason.as_deref().unwrap_or("MISSING"),
        optional(request.error.as_deref(), |e| format!("\n  {e}"))
    );
    for evicted in &request.evicted {
        println!(
            "  eviction planned: {} ({} apps)",
            evicted.udid.as_deref().unwrap_or(&evicted.id),
            evicted
                .installed_apps
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".into())
        );
    }
}
