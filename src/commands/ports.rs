//! CLI port reservations and interactive conflict suggestions.
use super::output;
use crate::cli::PortCommand;
use crate::{
    client,
    paths::Paths,
    protocol::{Body, Method},
    ui,
};
use anyhow::Result;
use serde_json::json;

pub(super) async fn run(paths: &Paths, command: PortCommand, json_output: bool) -> Result<i32> {
    match command {
        PortCommand::Reserve {
            name,
            workspace,
            mut port,
            mut env,
            mut reason,
            on_conflict,
        } => {
            let workspace = ui::workspace(paths, workspace, true, json_output).await?;
            loop {
                match client::call(
                    paths,
                    Method::ReservePort {
                        workspace: workspace.clone(),
                        name: name.clone(),
                        port,
                        env_var: env.clone(),
                        reason: reason.clone(),
                        on_conflict,
                    },
                )
                .await?
                {
                    Body::Port(reservation) => {
                        output(
                            json_output,
                            &format!(
                                "{}={} ({})",
                                reservation.name, reservation.port, reservation.env_var
                            ),
                            serde_json::to_value(reservation)?,
                        );
                        break;
                    }
                    Body::PortSuggestion(proposal) => {
                        if json_output || !ui::is_interactive(json_output) {
                            let mut value = serde_json::to_value(&proposal)?;
                            value["reserved"] = json!(false);
                            output(
                                json_output,
                                &format!(
                                    "{}: port {} unavailable; suggested {}. Accept with --port {}",
                                    proposal.name,
                                    proposal.requested_port,
                                    proposal.suggested_port,
                                    proposal.suggested_port
                                ),
                                value,
                            );
                            return Ok(2);
                        }
                        println!(
                            "{}: port {} unavailable; suggested {}",
                            proposal.name, proposal.requested_port, proposal.suggested_port
                        );
                        if !ui::confirm(
                            &format!("Reserve suggested port {}?", proposal.suggested_port),
                            json_output,
                            &format!("--port {}", proposal.suggested_port),
                        )? {
                            return Ok(2);
                        }
                        port = Some(proposal.suggested_port);
                        env = Some(proposal.env_var);
                        reason = proposal.reason;
                    }
                    _ => anyhow::bail!("unexpected port response"),
                }
            }
        }
        PortCommand::List { workspace, all } => {
            let workspace = if all {
                None
            } else {
                Some(ui::workspace(paths, workspace, true, json_output).await?)
            };
            let ports = match client::call(paths, Method::Ports { workspace }).await? {
                Body::Ports(ports) => ports,
                _ => anyhow::bail!("unexpected port list response"),
            };
            if json_output {
                println!("{}", serde_json::to_string(&ports)?);
            } else {
                let workspaces = ui::workspaces(paths).await?;
                for port in ports {
                    let owner = workspaces
                        .iter()
                        .find(|w| w.id == port.workspace_id)
                        .map(|w| w.name.as_str())
                        .unwrap_or(&port.workspace_id);
                    println!(
                        "{owner}/{}={} ({}){}",
                        port.name,
                        port.port,
                        port.env_var,
                        port.reason
                            .as_ref()
                            .map(|r| format!("  {r}"))
                            .unwrap_or_default()
                    );
                }
            }
        }
        PortCommand::Release { name, workspace } => {
            let workspace = ui::workspace(paths, workspace, true, json_output).await?;
            client::call(paths, Method::ReleasePort { workspace, name }).await?;
            output(
                json_output,
                "Port reservation released",
                json!({"released":true}),
            );
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
    let overview = match client::call(paths, Method::PortOverview { workspace }).await? {
        Body::PortOverview(overview) => overview,
        _ => anyhow::bail!("unexpected port overview response"),
    };
    if json_output {
        println!("{}", serde_json::to_string(&overview)?);
    } else {
        for p in &overview.reserved {
            println!(
                "{}={} ({}){}",
                p.name,
                p.port,
                p.env_var,
                p.reason
                    .as_ref()
                    .map(|r| format!("  {r}"))
                    .unwrap_or_default()
            );
        }
        for (name, definition) in &overview.configured {
            if !overview.reserved.iter().any(|p| &p.name == name) {
                println!(
                    "{name}: not reserved (preferred: {}; conflicts: {:?})",
                    definition
                        .port
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "automatic".into()),
                    definition.on_conflict.unwrap_or(overview.on_conflict)
                );
            }
        }
        if overview.reserved.is_empty() && overview.configured.is_empty() {
            println!("No configured or reserved ports");
        }
    }
    Ok(0)
}
