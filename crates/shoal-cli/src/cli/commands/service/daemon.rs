//! Shoal's own status, which `shoal status` shows outside a space; `shoal
//! daemon start`, `stop`, `reload` and `restart`; and stopping a daemon
//! that does not answer.
use anyhow::{Result, ensure};

use super::install::install;
use crate::client::output::daemon::{Reloaded, Running, Status};
use crate::{
    cli::{client, context::Context, output::Style, ui},
    paths::Paths,
    protocol::Exit,
    service::{self, Platform},
};

pub(in crate::cli::commands) async fn status(ctx: &Context) -> Result<i32> {
    let status = client::status(&ctx.paths).await?;
    let running = status.is_some();
    let message = status
        .as_ref()
        .map(status_message)
        .unwrap_or_else(|| "Daemon is not running".into());
    let healthy = status
        .as_ref()
        .is_some_and(|s| s.access_stalls.is_empty() && s.cleanup_error.is_none());
    ctx.emit_styled(
        if healthy {
            Style::Success
        } else {
            Style::Warning
        },
        &message,
        Status {
            running,
            daemon: status,
            socket: ctx.paths.socket.clone(),
        },
    )?;
    Ok(if running { 0 } else { 1 })
}

/// The running daemon, each directory whose file access stalls, and why
/// automatic cleanup fails other than in a workspace.
fn status_message(status: &crate::protocol::DaemonStatus) -> String {
    let mut message = format!(
        "Daemon running (PID {}, version {}{})",
        status.pid,
        status.version,
        match status.unread_notifications {
            0 => String::new(),
            n => format!(", {n} new notifications"),
        }
    );
    let now = crate::time::UnixTime::now();
    for stall in &status.access_stalls {
        let since = stall.since.local_ago(now);
        message.push_str(&format!("\n{} (since {since})", stall.message()));
    }
    if let Some(error) = &status.cleanup_error {
        message.push_str(&format!("\n{error}"));
    }
    message
}

pub(super) async fn start(ctx: &Context) -> Result<i32> {
    if !service::file(&ctx.paths, Platform::current()?).is_file()
        && ui::offer(
            ctx,
            "The daemon service is not installed. Install it with `shoal daemon install`?",
        )?
    {
        return install(ctx, false, None).await;
    }
    start_service(ctx).await?;
    let started = Running {
        running: true,
        stopped_work: None,
    };
    ctx.emit_styled(Style::Success, "Daemon started", started)?;
    Ok(0)
}

pub(super) async fn stop(ctx: &Context) -> Result<i32> {
    ctx.progress("Stopping daemon", stop_daemon(&ctx.paths, Exit::Stop))
        .await?;
    emit_stopped_work(ctx, "Daemon stopped", false)?;
    Ok(0)
}

pub(super) async fn reload(ctx: &Context) -> Result<i32> {
    ensure!(
        client::reload_config(&ctx.paths).await?,
        "the daemon is not running; it reads the config when it starts"
    );
    ctx.emit_styled(
        Style::Success,
        "Daemon reloaded the global config",
        Reloaded { reloaded: true },
    )?;
    Ok(0)
}

pub(super) async fn restart(ctx: &Context) -> Result<i32> {
    // Service administration must still work after a protocol upgrade.
    if let Ok(Some(status)) = ctx
        .progress("Checking daemon", client::status(&ctx.paths))
        .await
    {
        ensure!(
            status.managed,
            "foreground daemon: stop it and run `shoal daemon run` again"
        );
    }
    ctx.progress("Stopping daemon", stop_daemon(&ctx.paths, Exit::Restart))
        .await?;
    start_service(ctx).await?;
    emit_stopped_work(ctx, "Daemon restarted", true)?;
    Ok(0)
}

/// Stopping saves running agents and commands, as does a restart for those
/// that cannot reattach; point to restoring them.
fn emit_stopped_work(ctx: &Context, message: &str, running: bool) -> Result<()> {
    let stopped = crate::execution::recovery::any_pending(&ctx.paths);
    let message = if stopped {
        format!("{message}; restore stopped work with shoal resume --all")
    } else {
        message.to_owned()
    };
    ctx.emit_styled(
        Style::Success,
        &message,
        Running {
            running,
            stopped_work: Some(stopped),
        },
    )
}

async fn start_service(ctx: &Context) -> Result<()> {
    ctx.progress("Starting daemon service", service::start(&ctx.paths))
        .await?;
    ctx.progress("Waiting for daemon", client::wait(&ctx.paths, true))
        .await
}

/// Ask the daemon to exit with `exit`, then unload its service. A daemon that
/// does not answer gets SIGTERM, which stops its work saved first.
pub(super) async fn stop_daemon(paths: &Paths, exit: Exit) -> Result<()> {
    if !client::exit(paths, exit).await? {
        return terminate(paths).await;
    }
    client::wait(paths, false).await?;
    // The service manager keeps a daemon's service loaded after it exits.
    if service::installed(paths) {
        service::stop(paths).await?;
    }
    Ok(())
}

/// Stop a daemon with SIGTERM through the OS service manager, or directly
/// when it runs in the foreground or does not answer status without an
/// installed service, as while it starts.
async fn terminate(paths: &Paths) -> Result<()> {
    match client::status(paths).await {
        Ok(Some(status)) if !status.managed => {
            signal_owner(
                paths,
                anyhow::anyhow!("the foreground daemon does not answer"),
            )?;
        }
        Err(error) if !service::file(paths, Platform::current()?).exists() => {
            signal_owner(paths, error)?;
        }
        _ => service::stop(paths).await?,
    }
    client::wait(paths, false).await
}

/// SIGTERM to the daemon owning the state directory; `missing` when none does.
fn signal_owner(paths: &Paths, missing: anyhow::Error) -> Result<()> {
    let owner = crate::daemon_owner::owner(paths).ok_or(missing)?;
    crate::process::identity::signal(&owner, libc::SIGTERM)
}

#[cfg(test)]
mod tests;
