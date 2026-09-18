//! CLI requests and rendering for cooperative resources.
use anyhow::{Result, bail};
use serde_json::json;

use super::{Attempt, EXIT_BUSY, retry_while_busy};
use crate::{
    cli::ResourceCommand,
    client::{self, request},
    context::{Context, optional},
    output::{Palette, Style},
    protocol::{Body, Method},
    resources::{Overview, ResourceKind, ResourceLease, ResourceRequest},
    ui::{self, Fallback},
};

pub(super) async fn run(ctx: &Context, command: ResourceCommand) -> Result<i32> {
    match command {
        ResourceCommand::Acquire {
            mode,
            pool,
            workspace,
            resource,
            name,
            reason,
            wait,
        } => {
            let workspace =
                ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
            let request = ResourceRequest {
                mode,
                pool,
                resource,
                name,
                reason,
            };
            let outcome = retry_while_busy(wait, async || {
                let method = Method::ResourceAcquire {
                    workspace: workspace.clone(),
                    request: request.clone(),
                };
                Ok(match client::call(&ctx.paths, method).await? {
                    Body::ResourceLease(lease) => Attempt::Ready(lease),
                    Body::ResourceBusy { message } => Attempt::Busy(message),
                    _ => bail!("unexpected resource acquisition response"),
                })
            })
            .await?;
            match outcome {
                Ok(lease) => {
                    ctx.emit(&describe(&lease, Palette::stdout(ctx.json)), &lease)?;
                    Ok(0)
                }
                Err(message) => {
                    ctx.emit_styled(
                        Style::Warning,
                        &message,
                        json!({
                            "acquired": false,
                            "code": "resource_busy",
                            "pool": request.pool,
                            "resource": request.resource,
                            "message": message,
                        }),
                    )?;
                    Ok(EXIT_BUSY)
                }
            }
        }
        ResourceCommand::Release {
            pool,
            workspace,
            name,
        } => {
            let workspace =
                ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
            let method = Method::ResourceRelease {
                workspace,
                pool,
                name,
            };
            client::call(&ctx.paths, method).await?;
            ctx.emit_styled(
                Style::Success,
                "Resource released",
                json!({"released": true}),
            )?;
            Ok(0)
        }
        ResourceCommand::List { workspace, all } => {
            let workspace = ui::select_workspace_filter(ctx, workspace, all).await?;
            let leases = request!(
                &ctx.paths,
                Method::ResourceList { workspace },
                ResourceLeases
            );
            ctx.show(&leases, |leases| {
                for lease in leases {
                    println!(
                        "{}  {}",
                        lease.workspace_id,
                        describe_short(lease, Palette::stdout(ctx.json))
                    );
                }
                if leases.is_empty() {
                    println!("No resource leases");
                }
            })?;
            Ok(0)
        }
    }
}

fn describe(lease: &ResourceLease, palette: Palette) -> String {
    format!(
        "{}/{} -> {} [{}] ({})",
        palette.paint(Style::Heading, &lease.pool),
        lease.name,
        lease.resource,
        lease.mode,
        lease.id
    )
}

fn describe_short(lease: &ResourceLease, palette: Palette) -> String {
    format!(
        "{}/{} -> {} [{}]{}",
        palette.paint(Style::Heading, &lease.pool),
        lease.name,
        lease.resource,
        lease.mode,
        optional(lease.reason.as_deref(), |r| format!(" ({r})"))
    )
}

pub(super) async fn overview(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let overview = request!(
        &ctx.paths,
        Method::ResourceOverview { workspace },
        ResourceOverview
    );
    ctx.show(&overview, |overview| {
        render_overview(overview, Palette::stdout(ctx.json))
    })?;
    Ok(0)
}

fn render_overview(overview: &Overview, palette: Palette) {
    for pool in &overview.pools {
        println!(
            "{} ({}) {}/{} in use, {} available{}",
            palette.paint(Style::Heading, &pool.name),
            pool.scope,
            pool.used,
            pool.capacity,
            pool.available,
            if pool.configuration_matches {
                String::new()
            } else {
                palette.paint(
                    Style::Warning,
                    " [configuration changed; drain leases first]",
                )
            }
        );
        for resource in &pool.resources {
            if resource.kind == ResourceKind::Rwlock {
                println!(
                    "  {}: {} readers, {} writers; read available: {}, write available: {}",
                    resource.name,
                    resource.readers,
                    resource.writers,
                    resource.read_available,
                    resource.write_available
                );
            } else {
                println!(
                    "  {}: {}/{} in use, {} available",
                    resource.name, resource.used, resource.capacity, resource.available
                );
            }
        }
    }
    for lease in &overview.leases {
        println!("  lease {}", describe_short(lease, palette));
    }
    if overview.pools.is_empty() && overview.leases.is_empty() {
        println!("No configured resources or leases");
    }
}
