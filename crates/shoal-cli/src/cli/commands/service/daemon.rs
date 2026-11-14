//! Shoal's own status, which `shoal status` shows outside a space; `shoal
//! daemon start`, `stop` and `restart`, which wait for work in progress the
//! daemon's exit would cut off; and stopping a daemon that does not answer.
use anyhow::{Result, ensure};

use super::install::install;
use crate::client::connection::ExitAnswer;
use crate::client::output::daemon::{Kept, Running, Status};
use crate::{
    cli::{client, context::Context, output::Style, progress::Spinner, ui},
    paths::{CommandPaths, Paths},
    protocol::{Exit, ExitRequest, ExitWhen, Work},
    service::{self, Platform},
};

mod work;

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
        waited_for: Vec::new(),
    };
    ctx.emit_styled(Style::Success, "Daemon started", started)?;
    Ok(0)
}

pub(super) async fn stop(ctx: &Context, when: ExitWhen) -> Result<i32> {
    match stop_showing_work(ctx, Exit::Stop, when).await? {
        Stopping::Stopped {
            stopped_work,
            waited_for,
        } => {
            emit_stopped_work(ctx, "Daemon stopped", false, stopped_work, waited_for)?;
            Ok(0)
        }
        Stopping::Refused(work) => kept(ctx, Exit::Stop, work),
    }
}

pub(super) async fn restart(ctx: &Context, when: ExitWhen) -> Result<i32> {
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
    let waited_for = match stop_showing_work(ctx, Exit::Restart, when).await? {
        Stopping::Stopped { waited_for, .. } => waited_for,
        Stopping::Refused(work) => return kept(ctx, Exit::Restart, work),
    };
    start_service(ctx).await?;
    // The restart succeeded even when its saved work cannot be read.
    let stopped = crate::client::saved::waits_for_resume(&ctx.paths).await;
    emit_stopped_work(ctx, "Daemon restarted", true, stopped.ok(), waited_for)?;
    Ok(0)
}

/// Stopping saves running agents and commands, as does a restart for those
/// that cannot reattach; point to restoring them unless `stopped` says none
/// wait, as a daemon that does not say may have left some.
fn emit_stopped_work(
    ctx: &Context,
    message: &str,
    running: bool,
    stopped: Option<bool>,
    waited_for: Vec<Work>,
) -> Result<()> {
    let message = match stopped {
        Some(true) => format!("{message}; restore stopped work with shoal restore --all"),
        None => format!("{message}; restore any stopped work with shoal restore --all"),
        Some(false) => message.to_owned(),
    };
    ctx.emit_styled(
        Style::Success,
        &message,
        Running {
            running,
            stopped_work: stopped,
            waited_for,
        },
    )
}

/// Report that the daemon did not `exit`, since that would cut off `work`,
/// naming the commands that wait for it or cut it off; exit 2, as for other
/// busy requests.
fn kept(ctx: &Context, exit: Exit, work: Vec<Work>) -> Result<i32> {
    let (done, command) = match exit {
        Exit::Stop => ("stopped", "shoal daemon stop"),
        Exit::Restart => ("restarted", "shoal daemon restart"),
    };
    let message = format!(
        "Daemon not {done}: {} would be cut off; `{command}` waits for it, `{command} --force` cuts it off",
        work::names(&work)
    );
    let kept = Kept {
        running: true,
        blocked_by: work,
    };
    ctx.emit_styled(Style::Warning, &message, kept)?;
    Ok(2)
}

/// [`stop_daemon`] as `exit` and `when` say, below a spinner, with a line
/// naming the work in progress it waits for each time that changes. Work
/// that changes kind, as a workspace's preparation does when the workspace
/// turns ready, reads the same and gets no new line.
async fn stop_showing_work(ctx: &Context, exit: Exit, when: ExitWhen) -> Result<Stopping> {
    let (changed, mut changes) = tokio::sync::mpsc::unbounded_channel();
    let waiting = move |work: &[Work]| {
        let _ = changed.send(work::names(work));
    };
    let stopping = stop_daemon(&ctx.paths, ExitRequest { exit, when }, waiting);
    let mut stopping = std::pin::pin!(stopping);
    let mut spinner = Spinner::stderr(ctx.json, "Stopping daemon");
    let mut shown = String::new();
    loop {
        tokio::select! {
            stopped = &mut stopping => return stopped,
            Some(names) = changes.recv() => {
                if !ctx.json && names != shown {
                    spinner.clear();
                    eprintln!("Waiting for {names}");
                    shown = names;
                }
            }
            () = spinner.tick() => {}
        }
    }
}

async fn start_service(ctx: &Context) -> Result<()> {
    ctx.progress("Starting daemon service", service::start(&ctx.paths))
        .await?;
    ctx.progress("Waiting for daemon", client::wait(&ctx.paths, true))
        .await
}

/// What asking the daemon to exit did.
pub(super) enum Stopping {
    /// It exited, after the work in progress it waited for, saying whether
    /// it left stopped work when it says.
    Stopped {
        stopped_work: Option<bool>,
        waited_for: Vec<Work>,
    },
    /// It runs work in progress its exit would cut off, and keeps running.
    Refused(Vec<Work>),
}

/// Ask the daemon to exit as `request` says, then unload its service; what
/// it left, or the work that keeps it running when it refuses. `waiting`
/// hears the work in progress it waits for each time that changes. A daemon
/// that does not answer gets SIGTERM, which stops its work saved first; one
/// that went while it waited, as for an update, is asked again once the
/// daemon that follows it answers.
pub(super) async fn stop_daemon(
    paths: &CommandPaths,
    request: ExitRequest,
    mut waiting: impl FnMut(&[Work]),
) -> Result<Stopping> {
    let mut waited_for = Vec::new();
    let exiting = loop {
        let answer = client::exit(paths, request, &mut waiting).await?;
        // Only the daemon asked to exit knows a granted scope token, so no
        // later request carries it, to that daemon's successor least of all.
        crate::env::release_grant();
        match answer {
            ExitAnswer::Exiting(exiting) => break exiting,
            ExitAnswer::Refused(work) => return Ok(Stopping::Refused(work)),
            ExitAnswer::Unanswered => {
                terminate(paths).await?;
                let stopped_work = None;
                return Ok(Stopping::Stopped {
                    stopped_work,
                    waited_for,
                });
            }
            ExitAnswer::Gone(work) => {
                Work::list_new(&mut waited_for, work);
                if client::wait(paths, true).await.is_err() {
                    return stopped_without_successor(paths, waited_for).await;
                }
            }
        }
    };
    Work::list_new(&mut waited_for, exiting.waited_for.clone());
    let stopped_work = exiting.stopped_work().await;
    client::wait(paths, false).await?;
    // The service manager keeps a daemon's service loaded after it exits.
    if service::installed(paths) {
        service::stop(paths).await?;
    }
    Ok(Stopping::Stopped {
        stopped_work,
        waited_for,
    })
}

/// The stop of a daemon that went while it waited and that no daemon
/// followed, with its service unloaded so none starts.
async fn stopped_without_successor(
    paths: &CommandPaths,
    waited_for: Vec<Work>,
) -> Result<Stopping> {
    if service::installed(paths) {
        service::stop(paths).await?;
    }
    let stopped_work = None;
    Ok(Stopping::Stopped {
        stopped_work,
        waited_for,
    })
}

/// Stop a daemon with SIGTERM through the OS service manager, or directly
/// when it runs in the foreground or does not answer status without an
/// installed service, as while it starts.
async fn terminate(paths: &CommandPaths) -> Result<()> {
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
