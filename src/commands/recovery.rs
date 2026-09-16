//! CLI recovery reports and explicit repair requests.
use crate::recovery::Options;
use crate::{
    client,
    paths::Paths,
    protocol::{Body, Method},
    ui,
};
use anyhow::Result;

pub(super) async fn run(
    paths: &Paths,
    workspace: Option<String>,
    all: bool,
    options: Options,
    json_output: bool,
) -> Result<i32> {
    let workspace = if all {
        None
    } else {
        Some(ui::workspace(paths, workspace, true, json_output).await?)
    };
    let Body::Reconciliation(reports) =
        client::call(paths, Method::Reconcile { workspace, options }).await?
    else {
        anyhow::bail!("unexpected reconciliation response");
    };
    let unresolved = reports.iter().any(|r| !r.issues.is_empty());
    if json_output {
        println!("{}", serde_json::to_string(&reports)?);
    } else {
        for report in &reports {
            println!(
                "{}: {} ({:?})",
                report.workspace.name, report.workspace.state, report.directory
            );
            for execution in &report.executions {
                println!(
                    "  execution {}: {}{}{}",
                    execution.id,
                    execution.state,
                    if execution.connected {
                        " (connected)"
                    } else {
                        ""
                    },
                    if execution.cleared { " (cleared)" } else { "" }
                );
                for process in &execution.processes {
                    println!("    owned PID {}", process.pid);
                }
                for process in &execution.unverified_processes {
                    println!("    unverified group PID {} (not signaled)", process.pid);
                }
                for note in &execution.notes {
                    println!("    {note}");
                }
            }
            for change in &report.changes {
                println!("  repaired: {change}");
            }
            for issue in &report.issues {
                println!("  unresolved: {issue}");
            }
        }
        if reports.is_empty() {
            println!("No workspaces to reconcile");
        }
    }
    Ok(if unresolved { 2 } else { 0 })
}
