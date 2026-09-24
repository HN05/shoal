//! CLI requests and rendering for cooperative resources.
use anyhow::Result;
use serde_json::json;

use super::{
    EXIT_BUSY,
    acquisition::{Acquisition, retry},
    workspace_overviews,
};
use crate::{
    cli::{
        ResourceCommand, WorkspaceScope,
        client::{self, request},
        context::{Context, optional},
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::resources::{
        Overview, ResourceKind, ResourceLease, ResourceRequest, WorkspaceOverview,
    },
    protocol::{Body, Method},
};

pub(super) async fn run(
    ctx: &Context,
    command: Option<ResourceCommand>,
    scope: WorkspaceScope,
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
            let outcome = retry(wait, async || {
                let method = Method::ResourceAcquire {
                    workspace: workspace.clone(),
                    request: request.clone(),
                };
                Ok(match client::call(&ctx.paths, method).await? {
                    Body::ResourceLease(lease) => Acquisition::Acquired(lease),
                    Body::AccessRequest(request) => Acquisition::approval(request),
                    Body::Busy { message } => Acquisition::Busy(message),
                    body => {
                        return Err(body.unexpected("ResourceLease, AccessRequest or Busy"));
                    }
                })
            })
            .await?;
            match outcome {
                Acquisition::Acquired(lease) => {
                    ctx.emit(&describe(&lease, Palette::stdout(ctx.json)), &lease)?;
                    Ok(0)
                }
                Acquisition::ApprovalPending(request) | Acquisition::ApprovalDenied(request) => {
                    super::access::declined(ctx, &request)
                }
                Acquisition::Busy(message) => {
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
            client::request::<()>(&ctx.paths, method).await?;
            ctx.emit_styled(
                Style::Success,
                "Resource released",
                json!({"released": true}),
            )?;
            Ok(0)
        }
        Some(ResourceCommand::List { scope }) => overview(ctx, scope).await,
        None => overview(ctx, scope).await,
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

async fn overview(ctx: &Context, scope: WorkspaceScope) -> Result<i32> {
    if scope.all {
        return workspace_overviews(
            ctx,
            async |workspace| {
                let overview = request::<Overview>(
                    &ctx.paths,
                    Method::ResourceOverview {
                        workspace: workspace.id.clone(),
                    },
                )
                .await?;
                Ok(WorkspaceOverview {
                    workspace: workspace.clone(),
                    overview,
                })
            },
            |overview, palette| render_overview(&overview.overview, palette),
        )
        .await;
    } else {
        let workspace =
            ui::select_workspace(ctx, scope.workspace, Fallback::CurrentDirectory).await?;
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
