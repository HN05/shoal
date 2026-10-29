//! An agent the daemon paused: why it stopped, its saved recovery record, and
//! the wait for the daemon to restore it.
use std::{ffi::OsString, io::Write, path::PathBuf};

use anyhow::Result;

use super::{
    Mode,
    link::Link,
    recovery::{self, Restorer},
    takeover,
};
use crate::{
    model::{ExecutionPlan, PortReservation},
    paths::Paths,
    process,
    protocol::{ExecutionEvent, Protection},
};

/// How the wrapper continues after its agent paused.
pub(super) enum AfterPause {
    /// Report the stopped agent's exit code.
    Finish,
    /// The daemon resumed the agent: run `command` through `proxy`, from
    /// the same settings, with `ports`, consuming `record` once the restored
    /// child is registered.
    Restart {
        command: Vec<OsString>,
        proxy: Option<crate::proxy::Proxy>,
        ports: Vec<PortReservation>,
        record: Option<PathBuf>,
    },
    /// The daemon resumed the agent, which `installed`, the binary an
    /// upgrade put in place of this wrapper's, restores from `record`.
    TakeOver {
        installed: PathBuf,
        record: Option<PathBuf>,
    },
}

/// Save the paused agent's recovery record before reporting the pause, then,
/// when protection stopped it, wait for the daemon to restore it.
pub(super) async fn pause_agent(
    link: &mut Link,
    paths: &Paths,
    plan: &ExecutionPlan,
    mode: &Mode,
    recovery: &recovery::Recovery,
    stop: &Halt,
) -> Result<AfterPause> {
    let automatic = recovery.automatic && stop.recovers();
    let restorer = restorer(recovery, stop, automatic);
    let queued = matches!(restorer, Restorer::Queue(_));
    let saved = save_paused_agent(paths, plan, mode, recovery, stop, restorer);
    match &saved {
        // The terminal may have closed with the session.
        Ok(_) if queued => {
            let _ = writeln!(
                std::io::stderr(),
                "shoal: agent stopped{}; its restore waits in the agent queue",
                stop.stated_reason()
            );
        }
        Ok(_) if !automatic => {
            let restore = stop.manual_restore(&plan.workspace.name, &plan.id);
            let _ = writeln!(std::io::stderr(), "{restore}");
        }
        Ok(_) => {}
        Err(error) => eprintln!("warning: cannot save recovery command: {error:#}"),
    }
    if link.refusal().is_some() {
        return Ok(AfterPause::Finish);
    }
    let awaited = stop.protection().filter(|_| automatic);
    report_pause(link, awaited.clone()).await?;
    let Some(protection) = awaited else {
        return Ok(AfterPause::Finish);
    };
    eprintln!(
        "shoal: agent stopped{}; waiting until {}; the agent resumes automatically",
        stop.stated_reason(),
        protection.resumes_when
    );
    await_restore(link, paths, plan, recovery, stop, saved).await
}

/// Wait for the daemon to restore an agent protection stopped, restarting it
/// from its saved `record` when it resumes.
async fn await_restore(
    link: &mut Link,
    paths: &Paths,
    plan: &ExecutionPlan,
    recovery: &recovery::Recovery,
    stop: &Halt,
    record: Result<PathBuf>,
) -> Result<AfterPause> {
    match recovery::wait(link).await? {
        recovery::Waited::Resume { ports, executable } => {
            let record = record.ok();
            eprintln!("shoal: restoring agent session");
            if let Some(installed) = takeover::installed(link, executable) {
                return Ok(AfterPause::TakeOver { installed, record });
            }
            let (command, proxy) = recovery
                .restore_command(paths, &plan.workspace, record.as_deref())
                .await;
            Ok(AfterPause::Restart {
                command,
                proxy,
                ports,
                record,
            })
        }
        recovery::Waited::Cancelled { reason, removed } => {
            if record.is_ok() {
                let cancelled = reason.map(|reason| format!(": {reason}"));
                eprintln!(
                    "shoal: agent stopped{}; automatic restore cancelled{}; {}",
                    stop.stated_reason(),
                    cancelled.unwrap_or_default(),
                    restore_steps(&plan.workspace.name, &plan.id, removed)
                );
            }
            Ok(AfterPause::Finish)
        }
    }
}

/// What restores the paused agent besides `shoal resume`: this wrapper when
/// protection stopped it to restore it `automatic`ally, the agent queue when a
/// signal ended its session or the system shut down and its restore needs no
/// user, otherwise nothing.
fn restorer(recovery: &recovery::Recovery, stop: &Halt, automatic: bool) -> Restorer {
    if automatic {
        return this_wrapper().map_or(Restorer::User, Restorer::Wrapper);
    }
    match stop {
        Halt::Ended { .. } | Halt::Shutdown { .. } if recovery.automatic => {
            Restorer::Queue(crate::cli::terminal::start_environment())
        }
        _ => Restorer::User,
    }
}

/// The recovery record of a paused agent, naming where it ran and what
/// restores it.
fn save_paused_agent(
    paths: &Paths,
    plan: &ExecutionPlan,
    mode: &Mode,
    recovery: &recovery::Recovery,
    stop: &Halt,
    restorer: Restorer,
) -> Result<PathBuf> {
    recovery.save(
        paths,
        &plan.workspace.id,
        &plan.id,
        stop.saved_reason(),
        agent_place(mode),
        restorer,
    )
}

/// Save the recovery record of an agent before `stop`, the end of its session
/// or a system shutdown, stops it, as protection saves one before its stop:
/// either may kill this wrapper before the agent exits. Pausing the stopped
/// agent completes the record.
pub(super) fn save_ending_agent(
    paths: &Paths,
    plan: &ExecutionPlan,
    mode: &Mode,
    recovery: Option<&recovery::Recovery>,
    stop: &Halt,
) {
    let Some(recovery) = recovery else {
        return;
    };
    let saved = recovery.save_stopping(
        paths,
        &plan.workspace.id,
        &plan.id,
        stop.saved_reason(),
        agent_place(mode),
        restorer(recovery, stop, recovery.automatic && stop.recovers()),
    );
    if let Err(error) = saved {
        // The terminal may have closed with the session.
        let _ = writeln!(
            std::io::stderr(),
            "warning: cannot save recovery command: {error:#}"
        );
    }
}

/// Where the agent runs, as its terminal host names it. A detached agent
/// never ran in the launching terminal.
fn agent_place(mode: &Mode) -> Option<crate::sessions::Origin> {
    (!matches!(mode, Mode::Detached { .. }))
        .then(crate::cli::terminal::current_origin)
        .flatten()
}

/// Tell the daemon the agent paused. Awaiting a restore keeps the link armed
/// to reattach; otherwise the daemon may forget the stopped child.
async fn report_pause(link: &mut Link, awaited: Option<Protection>) -> Result<()> {
    match awaited {
        Some(protection) => link.await_restore(protection),
        None => link.disarm(),
    }
    link.send(&ExecutionEvent::Paused).await
}

/// Keep a stopped command for `shoal resume` to report.
pub(super) fn record_stopped_command(
    paths: &Paths,
    plan: &ExecutionPlan,
    command: &[OsString],
    stop: &Halt,
) {
    match recovery::save_command(paths, &plan.workspace.id, &plan.id, command) {
        Ok(_) => eprintln!(
            "shoal: command stopped{}; shoal resume {} lists it",
            stop.stated_reason(),
            plan.workspace.name
        ),
        Err(error) => eprintln!("warning: cannot record stopped command: {error:#}"),
    }
}

/// This wrapper, as a recovery record names the one restoring its agent.
fn this_wrapper() -> Option<process::identity::Identity> {
    process::identity::capture(std::process::id())
        .ok()
        .flatten()
}

/// Why the daemon stopped a command it expects to be restored.
#[derive(Debug)]
pub(super) enum Halt {
    /// Overload or disk space protection; the reason is saved for `shoal resume`.
    Protection {
        recover: bool,
        reason: String,
        resumes_when: String,
    },
    /// `shoal stop`, workspace removal, a daemon shutdown or a refused
    /// reattachment.
    Pause {
        reason: Option<String>,
        /// The removal record of the removed workspace keeps the session.
        removed: bool,
    },
    /// A hangup or terminate signal ended the session the command ran in, as
    /// closing its terminal, logging out or shutting down does; the reason is
    /// saved for `shoal resume`.
    Ended { reason: &'static str },
    /// The daemon stopped it before the system shut down; the reason is
    /// saved for `shoal resume`.
    Shutdown { reason: Option<String> },
}

impl Halt {
    fn recovers(&self) -> bool {
        matches!(self, Self::Protection { recover: true, .. })
    }

    /// The protection the daemon restores the agent after.
    fn protection(&self) -> Option<Protection> {
        match self {
            Self::Protection {
                reason,
                resumes_when,
                ..
            } => Some(Protection {
                reason: reason.clone(),
                resumes_when: resumes_when.clone(),
            }),
            Self::Pause { .. } | Self::Ended { .. } | Self::Shutdown { .. } => None,
        }
    }

    fn saved_reason(&self) -> Option<&str> {
        match self {
            Self::Protection { reason, .. } => Some(reason),
            Self::Ended { reason } => Some(reason),
            Self::Shutdown { reason } => reason.as_deref(),
            Self::Pause { .. } => None,
        }
    }

    /// The halt for a session ended by `signal`, which the wrapper received or
    /// which ended the command before the wrapper's own arrived.
    pub(super) fn ended_by(signal: i32) -> Option<Self> {
        let reason = match signal {
            libc::SIGHUP => "its terminal closed",
            libc::SIGTERM => "its session was terminated",
            _ => return None,
        };
        Some(Self::Ended { reason })
    }

    /// The cause for every line after the stop.
    fn stated_reason(&self) -> String {
        match self {
            Self::Protection { reason, .. }
            | Self::Pause {
                reason: Some(reason),
                ..
            }
            | Self::Shutdown {
                reason: Some(reason),
            } => format!(": {reason}"),
            Self::Ended { reason } => format!(": {reason}"),
            Self::Pause { reason: None, .. } | Self::Shutdown { reason: None } => String::new(),
        }
    }

    /// The line for an agent that does not restore itself.
    fn manual_restore(&self, workspace: &str, execution: &str) -> String {
        let condition = match self {
            Self::Protection { resumes_when, .. } => format!("once {resumes_when}, "),
            Self::Pause { .. } | Self::Ended { .. } | Self::Shutdown { .. } => String::new(),
        };
        format!(
            "shoal: agent stopped{}; {condition}{}",
            self.stated_reason(),
            restore_steps(
                workspace,
                execution,
                matches!(self, Self::Pause { removed: true, .. })
            )
        )
    }
}

/// How to restore the agent by hand: resume it, once its workspace is
/// restored when it was `removed`.
fn restore_steps(workspace: &str, execution: &str, removed: bool) -> String {
    let resume = format!("shoal resume {workspace} --execution {execution}");
    if removed {
        format!("restore with shoal work restore {workspace}, then {resume}")
    } else {
        format!("restore with {resume}")
    }
}

#[cfg(test)]
mod tests;
