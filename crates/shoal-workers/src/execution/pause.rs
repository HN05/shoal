//! An agent the daemon paused, and the wait for the daemon to restore it.
mod place;

use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::{ffi::OsString, io::Write, path::PathBuf};

use anyhow::Result;

use shoal_protocol::protocol::ScopeMemory;
use shoal_protocol::stop_causes::{EndSignal, SignalTarget, StopCause};

use super::{
    Mode,
    link::Link,
    recovery::{self, Restorer},
    takeover,
};
use crate::{
    client::connection::CommandPaths,
    config::permissions::PermissionsConfig,
    model::{ExecutionPlan, PortReservation, Protection},
    paths::Paths,
    process,
    protocol::ExecutionEvent,
};
use place::Key;
pub(super) use place::Place;

/// How the wrapper continues after its agent paused.
pub(super) enum AfterPause {
    /// Report the stopped agent's exit code.
    Finish,
    /// The daemon resumed the agent: run `command` through `proxy`, from
    /// the same settings, with `ports`, consuming `record` once the restored
    /// child is registered, under `narrowing` when the daemon changed it.
    Restart {
        command: Vec<OsString>,
        proxy: Option<shoal_core::proxy::Proxy>,
        ports: Vec<PortReservation>,
        record: Option<PathBuf>,
        narrowing: Option<PermissionsConfig>,
    },
    /// The daemon resumed the agent, which `installed`, the binary an
    /// upgrade put in place of this wrapper's, restores from `record`,
    /// under `narrowing` when the daemon changed it.
    TakeOver {
        installed: PathBuf,
        record: Option<PathBuf>,
        narrowing: Option<PermissionsConfig>,
    },
}

/// Save the paused agent's recovery record before reporting the pause, with
/// what its scope recorded of its `memory`, then, when protection stopped
/// it, wait for the daemon to restore it.
pub(super) async fn pause_agent(
    link: &mut Link,
    paths: &CommandPaths,
    plan: &ExecutionPlan,
    mode: &Mode,
    recovery: &recovery::Recovery,
    stop: &Halt,
    memory: Option<ScopeMemory>,
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
            let restore = stop.manual_restore(&plan.space.id, &plan.id);
            let _ = writeln!(std::io::stderr(), "{restore}");
        }
        Ok(_) => {}
        Err(error) => eprintln!("warning: cannot save recovery command: {error:#}"),
    }
    if link.refusal().is_some() {
        return Ok(AfterPause::Finish);
    }
    let awaited = stop.protection().filter(|_| automatic);
    report_pause(link, awaited.clone(), memory, stop.cause()).await?;
    let Some(protection) = awaited else {
        return Ok(AfterPause::Finish);
    };
    let place = Place::show(&protection);
    await_restore(link, paths, plan, recovery, stop, saved, place).await
}

/// Wait in `place` for the daemon to restore an agent protection stopped,
/// restarting it from its saved `record` when it resumes.
async fn await_restore(
    link: &mut Link,
    paths: &CommandPaths,
    plan: &ExecutionPlan,
    recovery: &recovery::Recovery,
    stop: &Halt,
    record: Result<PathBuf>,
    mut place: Place,
) -> Result<AfterPause> {
    let waited = wait(link, &mut place).await?;
    // The terminal goes back to the restored agent, or to the shell.
    drop(place);
    match waited {
        Waited::Resume {
            ports,
            executable,
            narrowing,
        } => {
            let record = record.ok();
            eprintln!("shoal: restoring agent session");
            if let Some(installed) = takeover::installed(link, executable) {
                return Ok(AfterPause::TakeOver {
                    installed,
                    record,
                    narrowing,
                });
            }
            let (command, proxy) =
                super::restore::restore_command(recovery, paths, &plan.space, record.as_deref())
                    .await;
            Ok(AfterPause::Restart {
                command,
                proxy,
                ports,
                record,
                narrowing,
            })
        }
        Waited::Ended(ended) => {
            leave_to_queue(paths, plan, ended, record.is_ok());
            Ok(AfterPause::Finish)
        }
        Waited::Cancelled { reason, removed } => {
            if record.is_ok() {
                let cancelled = reason.map(|reason| format!(": {reason}"));
                eprintln!(
                    "shoal: agent stopped{}; automatic restore cancelled{}; {}",
                    stop.stated_reason(),
                    cancelled.unwrap_or_default(),
                    restore_steps(&plan.space.id, &plan.id, removed)
                );
            }
            Ok(AfterPause::Finish)
        }
    }
}

/// Leave the restore of the agent this wrapper waited for, whose session
/// `ended`, to the agent queue. The queue starts it in a new place once
/// this execution has ended, as for an agent whose session ends while it
/// runs. Without a `saved` record there is nothing to restore.
fn leave_to_queue(paths: &CommandPaths, plan: &ExecutionPlan, ended: Halt, saved: bool) {
    if !saved {
        return;
    }
    let env = crate::terminal::start_environment();
    let requested = recovery::request_restore(paths, &plan.space.id, &plan.id, &env);
    // The terminal may have closed with the session.
    let _ = match requested {
        Ok(()) => writeln!(
            std::io::stderr(),
            "shoal: agent stopped{}; its restore waits in the agent queue",
            ended.stated_reason()
        ),
        Err(error) => writeln!(
            std::io::stderr(),
            "warning: cannot queue the agent's restore: {error:#}"
        ),
    };
}

/// What restores the paused agent besides `shoal resume`. This wrapper does
/// when protection stopped the agent to restore it `automatic`ally. The
/// agent queue does when a signal ended its session or killed it, or the
/// system shut down, and its restore needs no user. Otherwise nothing does.
fn restorer(recovery: &recovery::Recovery, stop: &Halt, automatic: bool) -> Restorer {
    if automatic {
        return this_wrapper().map_or(Restorer::User, Restorer::Wrapper);
    }
    match stop {
        Halt::Ended { .. } | Halt::Killed { .. } | Halt::Shutdown { .. } if recovery.automatic => {
            Restorer::Queue(crate::terminal::start_environment())
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
        &plan.space.id,
        &plan.id,
        stop.ended(),
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
        &plan.space.id,
        &plan.id,
        stop.ended(),
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
        .then(crate::terminal::current_origin)
        .flatten()
}

/// Tell the daemon the agent paused, with what its scope recorded of its
/// `memory` and the `cause` of an end the daemon did not ask for. Awaiting a
/// restore keeps the link armed to reattach; otherwise the daemon may forget
/// the stopped child.
async fn report_pause(
    link: &mut Link,
    awaited: Option<Protection>,
    memory: Option<ScopeMemory>,
    cause: Option<StopCause>,
) -> Result<()> {
    match awaited {
        Some(protection) => link.await_restore(protection),
        None => link.disarm(),
    }
    link.send(&ExecutionEvent::Paused { memory, cause }).await
}

/// Keep a stopped command for `shoal resume` to report.
pub(super) fn record_stopped_command(
    paths: &Paths,
    plan: &ExecutionPlan,
    command: &[OsString],
    stop: &Halt,
) {
    match recovery::save_command(paths, &plan.space.id, &plan.id, command) {
        Ok(_) => eprintln!(
            "shoal: command stopped{}; shoal resume {} lists it",
            stop.stated_reason(),
            plan.space.number
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
    /// reattachment; the reason is saved for `shoal resume`.
    Pause {
        reason: Option<String>,
        /// The removal record of the removed workspace keeps the session.
        removed: bool,
    },
    /// A hangup or terminate signal ended the session the command ran in, as
    /// closing its terminal, logging out or shutting down does, `reached`
    /// first by the wrapper or the command alone; the reason is saved for
    /// `shoal resume`.
    Ended {
        signal: EndSignal,
        reached: SignalTarget,
    },
    /// SIGKILL ended the agent, from the kernel's OOM killer when `oom`;
    /// the reason is saved for `shoal resume`.
    Killed { oom: bool },
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
            Self::Pause { .. }
            | Self::Ended { .. }
            | Self::Killed { .. }
            | Self::Shutdown { .. } => None,
        }
    }

    fn saved_reason(&self) -> Option<&str> {
        match self {
            Self::Protection { reason, .. } => Some(reason),
            Self::Ended { signal, .. } => Some(ended_reason(*signal)),
            Self::Killed { oom } => Some(killed_reason(*oom)),
            Self::Shutdown { reason } | Self::Pause { reason, .. } => reason.as_deref(),
        }
    }

    /// Why the agent stopped, as its recovery record keeps it. The record
    /// itself names the cause of an end the daemon did not ask for, so the
    /// stop is known, an OOM kill's restore waiting for memory, even when the
    /// daemon never hears of it.
    fn ended(&self) -> recovery::RecordedEnd<'_> {
        recovery::RecordedEnd {
            reason: self.saved_reason(),
            cause: self.cause(),
        }
    }

    /// What ended the agent, when the wrapper ended it unasked; the daemon
    /// knows the cause of every stop it asked for.
    fn cause(&self) -> Option<StopCause> {
        match self {
            Self::Ended { signal, reached } => Some(StopCause::Signal {
                signal: *signal,
                reached: *reached,
            }),
            Self::Killed { oom: true } => Some(StopCause::OomKill),
            Self::Killed { oom: false } => Some(StopCause::Signal {
                signal: EndSignal::Kill,
                reached: SignalTarget::Agent,
            }),
            Self::Protection { .. } | Self::Pause { .. } | Self::Shutdown { .. } => None,
        }
    }

    /// The halt for a command `signal` ended before the wrapper received one,
    /// which counts as the OOM killer's once its scope's reads show a kill.
    pub(super) fn of_command(signal: i32) -> Option<Self> {
        match signal {
            libc::SIGKILL => Some(Self::Killed { oom: false }),
            signal => Self::ended_by(signal, SignalTarget::Agent),
        }
    }

    /// The halt for a session ended by `signal`, which `reached` the wrapper,
    /// or ended the command before the wrapper's own arrived.
    pub(super) fn ended_by(signal: i32, reached: SignalTarget) -> Option<Self> {
        let signal = match signal {
            libc::SIGHUP => EndSignal::Hangup,
            libc::SIGTERM => EndSignal::Terminate,
            _ => return None,
        };
        Some(Self::Ended { signal, reached })
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
            Self::Ended { signal, .. } => format!(": {}", ended_reason(*signal)),
            Self::Killed { oom } => format!(": {}", killed_reason(*oom)),
            Self::Pause { reason: None, .. } | Self::Shutdown { reason: None } => String::new(),
        }
    }

    /// The line for an agent that does not restore itself.
    fn manual_restore(&self, workspace: &SpaceId, execution: &ExecutionId) -> String {
        let condition = match self {
            Self::Protection { resumes_when, .. } => format!("once {resumes_when}, "),
            Self::Pause { .. }
            | Self::Ended { .. }
            | Self::Killed { .. }
            | Self::Shutdown { .. } => String::new(),
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

/// Why `signal` ended the session, which a restore tells the agent.
fn ended_reason(signal: EndSignal) -> &'static str {
    match signal {
        EndSignal::Hangup => "its terminal closed",
        EndSignal::Terminate => "its session was terminated",
        EndSignal::Kill => killed_reason(false),
    }
}

/// Why SIGKILL ended the agent, the OOM killer's when `oom`.
fn killed_reason(oom: bool) -> &'static str {
    match oom {
        true => "the OOM killer ended it",
        false => "it was killed",
    }
}

/// How to restore the agent by hand: restore it in the workspace `workspace`
/// selects by ID, since names select nothing, or when it was `removed`,
/// restore the workspace, which restores its agent with it.
fn restore_steps(workspace: &SpaceId, execution: &ExecutionId, removed: bool) -> String {
    if removed {
        format!("restore with shoal restore {workspace}")
    } else {
        format!("resume with shoal resume {workspace} --execution {execution}")
    }
}

/// How a stopped agent's wait for automatic recovery ended.
#[derive(Debug)]
pub(super) enum Waited {
    Resume {
        ports: Vec<crate::model::PortReservation>,
        /// The binary the daemon runs, when it says.
        executable: Option<crate::fsutil::FileIdentity>,
        /// The narrowing the agent restores under, when the daemon changed it.
        narrowing: Option<crate::config::permissions::PermissionsConfig>,
    },
    /// The session the wrapper waited in ended, as `Halt::Ended` says, so
    /// the agent queue restores the agent elsewhere.
    Ended(Halt),
    /// The restore was cancelled, with the daemon's reason if it gave one.
    Cancelled {
        reason: Option<String>,
        /// Removal cancelled it, and the removal record keeps the agent.
        removed: bool,
    },
}

impl Waited {
    fn cancelled(reason: impl Into<String>) -> Self {
        Self::Cancelled {
            reason: Some(reason.into()),
            removed: false,
        }
    }
}

/// Wait in `place` for the daemon's verdict, or for its keys: Enter wakes the
/// agent, Ctrl-C stops waiting. A hangup or terminate ends the session it
/// waits in, which leaves the restore to the agent queue. While the daemon
/// restarts, the link reattaches and the next daemon continues the wait.
pub(super) async fn wait(link: &mut Link, place: &mut Place) -> Result<Waited> {
    use super::link::{Received, Refused};
    use crate::protocol::Control;
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut quit = signal(SignalKind::quit())?;
    let signalled = || Ok(Waited::cancelled("interrupted"));
    let ended = |signal| {
        Ok(Waited::Ended(
            Halt::ended_by(signal, SignalTarget::Wrapper).expect("a session signal"),
        ))
    };
    loop {
        tokio::select! {
            received = link.recv() => match received {
                Ok(Received::Detached | Received::Reattached) => {}
                Ok(Received::Control(Control::Resume { ports, executable, narrowing })) => {
                    // The restored child reattaches once it is registered.
                    link.disarm();
                    return Ok(Waited::Resume { ports, executable, narrowing });
                }
                Ok(Received::Control(Control::Stop { reason, removed })) => {
                    return Ok(Waited::Cancelled { reason, removed });
                }
                Ok(Received::Control(Control::RestoreWaits { issue })) => {
                    place.note(format!("the restore also waits until the agent's remaining processes exit: {issue}"));
                }
                Ok(Received::Control(_)) => anyhow::bail!("unexpected recovery control"),
                Err(error) if error.is::<Refused>() => {
                    return Ok(Waited::cancelled(error.to_string()));
                }
                Err(error) => return Err(error),
            },
            key = place.key() => match key {
                Key::Wake => wake_now(link, place).await,
                Key::Stop => return signalled(),
            },
            _ = interrupt.recv() => return signalled(),
            _ = terminate.recv() => return ended(libc::SIGTERM),
            _ = hangup.recv() => return ended(libc::SIGHUP),
            _ = quit.recv() => return signalled(),
        }
    }
}

/// Ask the daemon over `link` to start the agent's restore now, which only
/// this execution's wrapper can. `place` shows that the agent wakes, or why
/// the daemon cannot be asked. The restore ends the wait with `Resume`.
async fn wake_now(link: &mut Link, place: &mut Place) {
    place.waking();
    if let Err(error) = link.send(&ExecutionEvent::Wake).await {
        place.wake_unsent(&error);
    }
}

#[cfg(test)]
mod tests;
