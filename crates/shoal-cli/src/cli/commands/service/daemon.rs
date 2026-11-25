//! Shoal's own status, which `shoal status` shows outside a space; `shoal
//! daemon start`, `stop` and `restart`, which wait for work in progress the
//! daemon's exit would cut off; and stopping a daemon that does not answer.
use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceNumber;

use super::install::install;
use crate::client::connection::ExitAnswer;
use crate::client::output::daemon::{Kept, Running, Status};
use crate::{
    cli::{client, context::Context, output::Style, progress::Spinner, ui},
    client::connection::CommandPaths,
    paths::Paths,
    protocol::{Exit, ExitRequest, ExitWhen, MemoryLimitSource, Work, WorkMemoryLimit},
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
    let healthy = status.as_ref().is_some_and(|s| {
        s.access_stalls.is_empty() && s.cleanup_error.is_none() && s.paused.is_none()
    });
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

/// The running daemon, since when the machine is paused, each directory
/// whose file access stalls, why automatic cleanup fails other than in a
/// workspace, and the limit on Shoal's work memory.
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
    if let Some(since) = status.paused {
        let since = since.local_minutes();
        message.push_str(&format!(
            "\nPaused since {since}: this machine admits no new work until shoal unpause"
        ));
    }
    let now = crate::time::UnixTime::now();
    for stall in &status.access_stalls {
        let since = stall.since.local_ago(now);
        message.push_str(&format!("\n{} (since {since})", stall.message()));
    }
    if let Some(error) = &status.cleanup_error {
        message.push_str(&format!("\n{error}"));
    }
    if let Some(role) = &status.hub {
        message.push_str(&format!("\n{}", crate::cli::commands::hub::role_line(role)));
    }
    message.push_str(&format!("\n{}", work_memory_line(&status.work_memory)));
    message
}

/// The limit on Shoal's work memory and what sets it, such as
/// `Work memory limit: 22.5 GiB, 75% of RAM, from load`.
fn work_memory_line(limit: &WorkMemoryLimit) -> String {
    let mut parts: Vec<String> = limit.bytes.map(ui::size_label).into_iter().collect();
    parts.extend(limit.percent.map(|percent| format!("{percent}% of RAM")));
    let source = match limit.source {
        MemoryLimitSource::Load if parts.is_empty() => return "Work memory limit: none".into(),
        MemoryLimitSource::Load => "load",
        MemoryLimitSource::Config => "memory_limit",
    };
    if parts.is_empty() {
        parts.push("none".into());
    }
    format!("Work memory limit: {}, from {source}", parts.join(", "))
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
        stopped_spaces: Vec::new(),
        waited_for: Vec::new(),
    };
    ctx.emit_styled(Style::Success, "Daemon started", started)?;
    Ok(0)
}

pub(super) async fn stop(ctx: &Context, when: ExitWhen) -> Result<i32> {
    match stop_showing_work(ctx, Exit::Stop, when).await? {
        Stopping::Stopped { left, waited_for } => {
            emit_stopped_work(ctx, "Daemon stopped", false, left, waited_for)?;
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
    let (left, waited_for) = match stop_showing_work(ctx, Exit::Restart, when).await? {
        Stopping::Stopped { left, waited_for } => (left, waited_for),
        Stopping::Refused(work) => return kept(ctx, Exit::Restart, work),
    };
    start_service(ctx).await?;
    emit_stopped_work(ctx, "Daemon restarted", true, left, waited_for)?;
    Ok(0)
}

/// What a daemon's exit left for `shoal resume`.
pub(super) enum Left {
    /// The stopped work of these spaces, by number, which the exit stopped.
    Spaces(Vec<SpaceNumber>),
    /// Not known, as from a daemon that does not say.
    Unknown,
}

impl Left {
    /// The command that restores what is left, while some is or may be:
    /// the spaces' own, so it restores nothing the exit did not stop.
    fn restore(&self) -> Option<String> {
        match self {
            Self::Spaces(spaces) if spaces.is_empty() => None,
            Self::Spaces(spaces) => {
                let numbers: Vec<String> = spaces.iter().map(ToString::to_string).collect();
                Some(format!(
                    "resume its stopped work with shoal resume {}",
                    numbers.join(" ")
                ))
            }
            Self::Unknown => Some("resume any stopped work with shoal resume".into()),
        }
    }
}

/// Stopping saves running agents and commands, as does a restart for those
/// that cannot reattach; point to restoring what is `left`.
fn emit_stopped_work(
    ctx: &Context,
    message: &str,
    running: bool,
    left: Left,
    waited_for: Vec<Work>,
) -> Result<()> {
    let message = match left.restore() {
        Some(restore) => format!("{message}; {restore}"),
        None => message.to_owned(),
    };
    let (stopped_work, stopped_spaces) = match left {
        Left::Spaces(spaces) => (Some(!spaces.is_empty()), spaces),
        Left::Unknown => (None, Vec::new()),
    };
    ctx.emit_styled(
        Style::Success,
        &message,
        Running {
            running,
            stopped_work,
            stopped_spaces,
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
    /// It exited, after the work in progress it waited for, leaving what it
    /// says it stopped.
    Stopped { left: Left, waited_for: Vec<Work> },
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
                let left = Left::Unknown;
                return Ok(Stopping::Stopped { left, waited_for });
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
    let left = exiting
        .stopped_spaces()
        .await
        .map_or(Left::Unknown, Left::Spaces);
    client::wait(paths, false).await?;
    // The service manager keeps a daemon's service loaded after it exits.
    if service::installed(paths) {
        service::stop(paths).await?;
    }
    Ok(Stopping::Stopped { left, waited_for })
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
    let left = Left::Unknown;
    Ok(Stopping::Stopped { left, waited_for })
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
