//! CLI requests and rendering for cooperative resources.
use anyhow::{Result, bail};
use serde_json::json;

use super::{Attempt, EXIT_BUSY, WorkspaceOverviewResult, retry_while_busy};
use crate::{
    cli::ResourceCommand,
    client::{self, request},
    context::{Context, optional},
    output::{Palette, Style},
    protocol::{Body, Method},
    resources::{Overview, ResourceKind, ResourceLease, ResourceRequest, WorkspaceOverview},
    ui::{self, Fallback},
};

pub(super) async fn run(
    ctx: &Context,
    command: Option<ResourceCommand>,
    workspace: Option<String>,
    all: bool,
) -> Result<i32> {
    match command {
        Some(ResourceCommand::Acquire {
            mode,
            pool,
            workspace,
            resource,
            name,
            reason,
            wait,
        }) => {
            let workspace =
                ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
            let request = ResourceRequest {
                mode,
                pool,
                resource,
                name,
                reason,
            };
            let mut approval = None;
            let outcome = retry_while_busy(wait, async || {
                let method = Method::ResourceAcquire {
                    workspace: workspace.clone(),
                    request: request.clone(),
                };
                approval = None;
                Ok(match client::call(&ctx.paths, method).await? {
                    Body::ResourceLease(lease) => Attempt::Ready(Ok(lease)),
                    Body::AccessRequest(request) => super::access::attempt(&mut approval, request),
                    Body::ResourceBusy { message } => Attempt::Busy(message),
                    _ => bail!("unexpected resource acquisition response"),
                })
            })
            .await?;
            match outcome {
                Ok(Ok(lease)) => {
                    ctx.emit(&describe(&lease, Palette::stdout(ctx.json)), &lease)?;
                    Ok(0)
                }
                Ok(Err(request)) => super::access::declined(ctx, &request),
                Err(_) if approval.is_some() => super::access::declined(ctx, &approval.unwrap()),
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
        Some(ResourceCommand::Release {
            pool,
            workspace,
            name,
        }) => {
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
        Some(ResourceCommand::List { workspace, all }) => overview(ctx, workspace, all).await,
        None => overview(ctx, workspace, all).await,
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

async fn overview(ctx: &Context, workspace: Option<String>, all: bool) -> Result<i32> {
    if all {
        let mut overviews = Vec::new();
        for workspace in client::workspaces(&ctx.paths).await? {
            let method = Method::ResourceOverview {
                workspace: workspace.id.clone(),
            };
            overviews.push(match client::call(&ctx.paths, method).await {
                Ok(Body::ResourceOverview(overview)) => {
                    WorkspaceOverviewResult::Ready(WorkspaceOverview {
                        workspace,
                        overview,
                    })
                }
                Ok(_) => WorkspaceOverviewResult::failed(
                    workspace,
                    "unexpected daemon response; expected ResourceOverview",
                ),
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
                        render_overview(&overview.overview, palette);
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
            request::<Overview>(&ctx.paths, Method::ResourceOverview { workspace }).await?;
        ctx.show(&overview, |overview| {
            render_overview(overview, Palette::stdout(ctx.json))
        })?;
    }
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
            if resource.requires_approval {
                println!(
                    "  {}: approval required ({})",
                    resource.name, resource.approval_lifetime
                );
            }
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
