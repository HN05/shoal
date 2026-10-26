//! CLI recovery reports and explicit repair requests.
use anyhow::Result;
use serde::Serialize;

use crate::{
    cli::{
        args::WorkspaceScope,
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui,
    },
    daemon::{
        doctor::{Check, CheckStatus},
        recovery::{ReconcileOptions, Report},
    },
    protocol::RecoveryMethod,
};

pub(super) async fn run(
    ctx: &Context,
    scope: WorkspaceScope,
    options: ReconcileOptions,
) -> Result<i32> {
    let (daemon, available) = daemon_check(ctx).await;
    let mut report = DoctorReport {
        checks: vec![daemon],
        workspaces: vec![],
    };
    if available {
        let checks = request::<Vec<Check>>(&ctx.paths, RecoveryMethod::Diagnose).await;
        match checks {
            Ok(checks) => report.checks.extend(checks),
            Err(error) => report.checks.push(Check::new(
                "environment",
                CheckStatus::Error,
                format!("Could not check daemon environment: {error:#}"),
            )),
        }
        match workspace_reports(ctx, scope, options).await {
            Ok(workspaces) => report.workspaces = workspaces,
            Err(error) => report.checks.push(Check::new(
                "workspaces",
                CheckStatus::Error,
                format!("Could not check workspaces: {error:#}"),
            )),
        }
    } else {
        report
            .checks
            .extend(crate::daemon::doctor::unavailable_checks());
    }
    report
        .checks
        .push(crate::daemon::environment::global_config_check(&ctx.paths));
    let shell_loaded =
        std::env::var_os(crate::env::SHELL_DIRECTIVE).is_some_and(|value| !value.is_empty());
    report.checks.push(Check::new(
        "shell_integration",
        if shell_loaded { CheckStatus::Ok } else { CheckStatus::Warning },
        if shell_loaded {
            "Shell integration is loaded in the calling shell".to_owned()
        } else {
            format!(
                "Shell integration is not loaded in the calling shell; `shoal cd` only prints a path. Run `{}`",
                crate::shell::INIT_COMMAND
            )
        },
    ));
    let unresolved = report.checks.iter().any(|c| c.status != CheckStatus::Ok)
        || report.workspaces.iter().any(|r| !r.issues.is_empty());
    ctx.show(&report, |report| {
        let palette = Palette::stdout(ctx.json);
        for check in &report.checks {
            let (label, style) = match check.status {
                CheckStatus::Ok => ("ok", Style::Success),
                CheckStatus::Warning => ("warning", Style::Warning),
                CheckStatus::Error => ("error", Style::Error),
                CheckStatus::Skipped => ("not checked", Style::Warning),
            };
            outln!(
                "{} {}: {}",
                palette.paint(style, label),
                check.name,
                check.message
            );
        }
        for workspace in &report.workspaces {
            render(workspace, palette);
        }
    })?;
    Ok(if unresolved { super::EXIT_BUSY } else { 0 })
}

#[derive(Serialize)]
struct DoctorReport {
    checks: Vec<Check>,
    workspaces: Vec<Report>,
}

async fn workspace_reports(
    ctx: &Context,
    scope: WorkspaceScope,
    options: ReconcileOptions,
) -> Result<Vec<Report>> {
    if scope.workspace.is_none() && !scope.all && client::workspaces(&ctx.paths).await?.is_empty() {
        return Ok(vec![]);
    }
    let workspace = ui::select_workspace_filter(ctx, scope).await?;
    request(&ctx.paths, RecoveryMethod::Doctor { workspace, options }).await
}

async fn daemon_check(ctx: &Context) -> (Check, bool) {
    let version = env!("CARGO_PKG_VERSION");
    let (status, message, available) = match client::status(&ctx.paths).await {
        Ok(Some(daemon)) if daemon.version == version => (
            CheckStatus::Ok,
            format!("Daemon running (PID {}, version {version})", daemon.pid),
            true,
        ),
        Ok(Some(daemon)) => (
            CheckStatus::Error,
            format!(
                "Daemon version {} differs from CLI {version}; restart the daemon with the installed CLI version",
                daemon.version
            ),
            false,
        ),
        Ok(None) => (
            CheckStatus::Error,
            format!(
                "Daemon is stopped or unreachable at {}; run `shoal daemon start` or `shoal daemon install`",
                ctx.paths.socket.display()
            ),
            false,
        ),
        Err(error) => (
            CheckStatus::Error,
            format!("Daemon is unreachable or incompatible: {error:#}"),
            false,
        ),
    };
    (Check::new("daemon", status, message), available)
}

fn render(report: &Report, palette: Palette) {
    outln!(
        "{}: {} ({})",
        palette.paint(Style::Heading, &report.workspace.name),
        palette.workspace_state(report.workspace.state),
        report.directory
    );
    for execution in &report.executions {
        outln!(
            "  execution {}: {}{}{}",
            execution.id,
            palette.execution_state(execution.state),
            if execution.connected {
                " (connected)"
            } else {
                ""
            },
            if execution.cleared { " (cleared)" } else { "" }
        );
        for process in &execution.processes {
            outln!("    owned PID {}", process.pid);
        }
        for process in &execution.unverified_processes {
            outln!("    unverified group PID {} (not signaled)", process.pid);
        }
        for process in &execution.unreadable_processes {
            outln!(
                "    unreadable environment PID {} (ownership unknown)",
                process.pid
            );
        }
        for note in &execution.notes {
            outln!("    {note}");
        }
    }
    for change in &report.changes {
        outln!("  {} {change}", palette.paint(Style::Success, "repaired:"));
    }
    for issue in &report.issues {
        outln!("  {} {issue}", palette.paint(Style::Warning, "unresolved:"));
    }
}
