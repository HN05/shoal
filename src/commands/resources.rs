//! CLI requests and rendering for cooperative resources.
use super::output;
use crate::{cli::ResourceCommand, resources};
use crate::{
    client,
    paths::Paths,
    protocol::{Body, Method},
    ui,
};
use anyhow::Result;
use serde_json::json;

pub(super) async fn run(paths: &Paths, command: ResourceCommand, json_output: bool) -> Result<i32> {
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
            let workspace = ui::workspace(paths, workspace, true, json_output).await?;
            let request = resources::AcquireRequest {
                mode,
                pool,
                resource,
                name,
                reason,
            };
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
            loop {
                match client::call(
                    paths,
                    Method::ResourceAcquire {
                        workspace: workspace.clone(),
                        request: request.clone(),
                    },
                )
                .await?
                {
                    Body::ResourceLease(lease) => {
                        output(
                            json_output,
                            &format!(
                                "{}/{} -> {} [{}] ({})",
                                lease.pool, lease.name, lease.resource, lease.mode, lease.id
                            ),
                            serde_json::to_value(&lease)?,
                        );
                        return Ok(0);
                    }
                    Body::ResourceBusy { message } if tokio::time::Instant::now() >= deadline => {
                        output(
                            json_output,
                            &message,
                            json!({"acquired":false,"code":"resource_busy","pool":request.pool,"resource":request.resource,"message":message}),
                        );
                        return Ok(2);
                    }
                    Body::ResourceBusy { .. } => {
                        tokio::time::sleep(
                            std::time::Duration::from_secs(1).min(
                                deadline.saturating_duration_since(tokio::time::Instant::now()),
                            ),
                        )
                        .await
                    }
                    _ => anyhow::bail!("unexpected resource acquisition response"),
                }
            }
        }
        ResourceCommand::Release {
            pool,
            workspace,
            name,
        } => {
            let workspace = ui::workspace(paths, workspace, true, json_output).await?;
            client::call(
                paths,
                Method::ResourceRelease {
                    workspace,
                    pool,
                    name,
                },
            )
            .await?;
            output(json_output, "Resource released", json!({"released":true}));
        }
        ResourceCommand::List { workspace, all } => {
            let workspace = if all {
                None
            } else {
                Some(ui::workspace(paths, workspace, true, json_output).await?)
            };
            let Body::ResourceLeases(leases) =
                client::call(paths, Method::ResourceList { workspace }).await?
            else {
                anyhow::bail!("unexpected resource list response");
            };
            if json_output {
                println!("{}", serde_json::to_string(&leases)?);
            } else {
                for lease in &leases {
                    println!(
                        "{}  {}/{} -> {} [{}]{}",
                        lease.workspace_id,
                        lease.pool,
                        lease.name,
                        lease.resource,
                        lease.mode,
                        lease
                            .reason
                            .as_ref()
                            .map(|r| format!(" ({r})"))
                            .unwrap_or_default()
                    );
                }
                if leases.is_empty() {
                    println!("No resource leases");
                }
            }
        }
    }
    Ok(0)
}

pub(super) async fn overview(
    paths: &Paths,
    workspace: Option<String>,
    json_output: bool,
) -> Result<i32> {
    let workspace = ui::workspace(paths, workspace, true, json_output).await?;
    let Body::ResourceOverview(overview) =
        client::call(paths, Method::ResourceOverview { workspace }).await?
    else {
        anyhow::bail!("unexpected resource overview response");
    };
    if json_output {
        println!("{}", serde_json::to_string(&overview)?);
    } else {
        for pool in &overview.pools {
            println!(
                "{} ({}) {}/{} in use, {} available{}",
                pool.name,
                pool.scope,
                pool.used,
                pool.capacity,
                pool.available,
                if pool.configuration_matches {
                    ""
                } else {
                    " [configuration changed; drain leases first]"
                }
            );
            for resource in &pool.resources {
                if resource.kind == resources::ResourceKind::Rwlock {
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
            println!(
                "  lease {}/{} -> {} [{}]{}",
                lease.pool,
                lease.name,
                lease.resource,
                lease.mode,
                lease
                    .reason
                    .as_ref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            );
        }
        if overview.pools.is_empty() && overview.leases.is_empty() {
            println!("No configured resources or leases");
        }
    }
    Ok(0)
}
