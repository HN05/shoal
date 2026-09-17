//! CLI recovery reports and explicit repair requests.
use anyhow::Result;

use crate::{
    client::request,
    context::Context,
    protocol::Method,
    recovery::{ReconcileOptions, Report},
    ui,
};

pub(super) async fn run(
    ctx: &Context,
    workspace: Option<String>,
    all: bool,
    options: ReconcileOptions,
) -> Result<i32> {
    let workspace = ui::select_workspace_filter(ctx, workspace, all).await?;
    let reports = request!(
        &ctx.paths,
        Method::Reconcile { workspace, options },
        Reconciliation
    );
    let unresolved = reports.iter().any(|r| !r.issues.is_empty());
    ctx.show(&reports, |reports| {
        for report in reports {
            render(report);
        }
        if reports.is_empty() {
            println!("No workspaces to reconcile");
        }
    })?;
    Ok(if unresolved { super::EXIT_BUSY } else { 0 })
}

fn render(report: &Report) {
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
