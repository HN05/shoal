//! CLI simulator requests, retry policy, and audit rendering.
use super::output;
use crate::{cli::SimCommand, simulators};
use crate::{
    client,
    paths::Paths,
    protocol::{Body, Method},
    ui,
};
use anyhow::Result;
use serde_json::json;

pub(super) async fn run(paths: &Paths, command: SimCommand, json_output: bool) -> Result<i32> {
    match command {
        SimCommand::Catalog => {
            let Body::SimCatalog(catalog) = client::call(paths, Method::SimCatalog).await? else {
                anyhow::bail!("unexpected simulator catalog response");
            };
            if json_output {
                println!("{catalog}");
            } else {
                println!("{}", serde_json::to_string_pretty(&catalog)?);
            }
        }
        SimCommand::List { workspace, all } => {
            let workspace = if all {
                None
            } else {
                Some(ui::workspace(paths, workspace, true, json_output).await?)
            };
            let Body::Simulators(sims) = client::call(paths, Method::SimList { workspace }).await?
            else {
                anyhow::bail!("unexpected simulator list response");
            };
            if json_output {
                println!("{}", serde_json::to_string(&sims)?);
            } else {
                for sim in &sims {
                    println!(
                        "{}  {}  {}  {}  {}",
                        sim.udid.as_deref().unwrap_or("pending"),
                        sim.lease_name.as_deref().unwrap_or("idle"),
                        sim.state,
                        sim.device,
                        sim.runtime
                    );
                }
                if sims.is_empty() {
                    println!("No managed simulators");
                }
            }
        }
        SimCommand::Acquire {
            workspace,
            name,
            profile,
            device,
            runtime,
            reason,
            clean,
            wait,
        } => {
            let workspace = ui::workspace(paths, workspace, true, json_output).await?;
            let request = simulators::SimRequest {
                request_id: uuid::Uuid::new_v4().to_string(),
                clean,
                name,
                profile,
                device,
                runtime,
                reason,
            };
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
            loop {
                match client::call(
                    paths,
                    Method::SimAcquire {
                        workspace: workspace.clone(),
                        request: request.clone(),
                    },
                )
                .await?
                {
                    Body::Simulator(sim) => {
                        output(
                            json_output,
                            &format!(
                                "{}={} ({}, {})",
                                sim.lease_name.as_deref().unwrap_or("default"),
                                sim.udid.as_deref().unwrap_or("pending"),
                                sim.device,
                                sim.runtime
                            ),
                            serde_json::to_value(&sim)?,
                        );
                        break;
                    }
                    Body::SimBusy { message } if tokio::time::Instant::now() >= deadline => {
                        output(
                            json_output,
                            &message,
                            json!({"acquired":false,"code":"simulator_busy","message":message}),
                        );
                        return Ok(2);
                    }
                    Body::SimBusy { .. } => {
                        tokio::time::sleep(
                            std::time::Duration::from_secs(1).min(
                                deadline.saturating_duration_since(tokio::time::Instant::now()),
                            ),
                        )
                        .await
                    }
                    _ => anyhow::bail!("unexpected simulator acquisition response"),
                }
            }
        }
        SimCommand::History {
            workspace,
            all,
            limit,
            before,
        } => {
            let workspace = if all {
                None
            } else {
                Some(ui::workspace(paths, workspace, true, json_output).await?)
            };
            let Body::SimHistory(entries) = client::call(
                paths,
                Method::SimHistory {
                    workspace,
                    limit,
                    before,
                },
            )
            .await?
            else {
                anyhow::bail!("unexpected simulator history response");
            };
            if json_output {
                println!("{}", serde_json::to_string(&entries)?);
            } else {
                for entry in &entries {
                    let r = &entry.request;
                    println!(
                        "#{} at {}  {}/{}  {}  action={}  erased-apps={}  actor={}\n  reason: {}{}",
                        entry.id,
                        r.requested_at,
                        r.workspace_name,
                        r.request.name,
                        r.status,
                        r.action.as_deref().unwrap_or("none"),
                        r.apps_removed
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "unknown/not erased".into()),
                        r.execution_id.as_deref().unwrap_or("unscoped caller"),
                        r.request.reason.as_deref().unwrap_or("MISSING"),
                        r.error
                            .as_ref()
                            .map(|e| format!("\n  {e}"))
                            .unwrap_or_default()
                    );
                    for evicted in &r.evicted {
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
                if entries.is_empty() {
                    println!("No clean-device requests");
                }
            }
        }
        SimCommand::Release { name, workspace } => {
            let workspace = ui::workspace(paths, workspace, true, json_output).await?;
            client::call(paths, Method::SimRelease { workspace, name }).await?;
            output(json_output, "Simulator released", json!({"released":true}));
        }
    }
    Ok(0)
}
