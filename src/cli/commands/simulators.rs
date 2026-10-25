//! CLI simulator requests, retry policy, and audit rendering.
use anyhow::Result;
use serde_json::json;

use super::acquisition::{Acquisition, obtain};
use crate::{
    cli::{
        WorkspaceScope,
        client::{self, request},
        context::{Context, optional},
        output::{Palette, Style},
        ui,
    },
    protocol::Method,
    sim::{SimRequest, Simulator, SimulatorCatalog, SimulatorOverview, audit::AuditEntry},
};

pub(super) async fn fetch_catalog(ctx: &Context, workspace: &str) -> Result<SimulatorCatalog> {
    let method = Method::SimCatalog {
        workspace: workspace.to_owned(),
    };
    request::<SimulatorCatalog>(&ctx.paths, method).await
}

pub(super) fn render_catalog(catalog: &SimulatorCatalog, palette: Palette) {
    let policy = &catalog.policy;
    outln!(
        "Capacity: {} booted, {} devices; idle devices deleted after {} seconds",
        policy.max_booted,
        policy.max_devices,
        policy.idle_seconds
    );
    if policy.allow_any {
        outln!("Devices outside the profiles: allowed with a reason");
    }
    for (name, profile) in &policy.profiles {
        let preferred = if catalog.preferred.contains(name) {
            " (preferred)"
        } else if policy.default.as_deref() == Some(name) {
            " (default)"
        } else {
            ""
        };
        outln!(
            "{}: {} / {}{}",
            palette.paint(Style::Heading, name),
            profile.device,
            if profile.runtime.is_empty() {
                "latest iOS"
            } else {
                &profile.runtime
            },
            preferred
        );
    }
    if policy.profiles.is_empty() {
        outln!("No configured profiles");
    }
    if let Some(error) = &catalog.inventory_error {
        outln!(
            "{}",
            palette.paint(
                Style::Warning,
                format!("Installed device types and runtimes unavailable: {error}")
            )
        );
        return;
    }
    outln!("{}", palette.paint(Style::Heading, "Runtimes"));
    for runtime in &catalog.runtimes {
        outln!("  {}  {}", runtime.name, runtime.identifier);
    }
    outln!("{}", palette.paint(Style::Heading, "Device types"));
    for device in &catalog.device_types {
        outln!("  {}  {}", device.name, device.identifier);
    }
}

pub(super) async fn history(
    ctx: &Context,
    scope: WorkspaceScope,
    limit: u32,
    before: Option<i64>,
) -> Result<i32> {
    let workspace = ui::select_workspace_filter(ctx, scope).await?;
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
            outln!("No clean-device requests");
        }
    })?;
    Ok(0)
}

/// Exit 2 means the simulator is busy or awaiting approval; a queued request
/// returns only once acquired or denied.
pub(super) async fn acquire(ctx: &Context, workspace: String, request: SimRequest) -> Result<i32> {
    let outcome = obtain(ctx, "a simulator", request.queue, async || {
        let method = Method::SimAcquire {
            workspace: workspace.clone(),
            request: request.clone(),
        };
        client::request::<Acquisition<Simulator>>(&ctx.paths, method).await
    })
    .await?;
    outcome.finish(
        ctx,
        |sim| ctx.emit_styled(Style::Success, &describe(&sim), &sim),
        |message| json!({"acquired": false, "code": "simulator_busy", "message": message}),
    )
}

pub(super) async fn overview(ctx: &Context, scope: WorkspaceScope) -> Result<i32> {
    let workspace = ui::select_workspace_filter(ctx, scope).await?;
    let overview =
        request::<SimulatorOverview>(&ctx.paths, Method::SimOverview { workspace }).await?;
    ctx.show(&overview, |overview| {
        render_overview(overview, Palette::stdout(ctx.json))
    })?;
    Ok(0)
}

pub(super) fn render_overview(overview: &SimulatorOverview, palette: Palette) {
    for sim in &overview.simulators {
        outln!(
            "{}  {}  {}  {}  {}",
            sim.udid.as_deref().unwrap_or("pending"),
            sim.lease_name.as_deref().unwrap_or("idle"),
            palette.simulator_state(sim.state),
            sim.device,
            sim.runtime
        );
    }
    if overview.simulators.is_empty() {
        outln!("No managed simulators");
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
    outln!(
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
        outln!(
            "  eviction planned: {} ({} apps)",
            evicted.udid.as_deref().unwrap_or(&evicted.id),
            evicted
                .installed_apps
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".into())
        );
    }
}
