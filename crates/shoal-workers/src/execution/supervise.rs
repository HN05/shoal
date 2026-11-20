//! Supervising a tracked command: registering its process group with the
//! daemon, handing it the terminal, and waiting for it to exit or be stopped.
use std::{
    ffi::OsString,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::ExitStatus,
};

use anyhow::{Context, Result, bail, ensure};
use tokio::{
    process::Child,
    signal::unix::{Signal, SignalKind, signal},
    sync::watch,
    time::timeout,
};

use super::{
    Mode,
    link::{Link, Received, Refused},
    pause::Halt,
    program_status::Reporter,
    recovery, scope_memory,
    signal_report::SignalReport,
    spawn::{AgentLaunch, spawn},
};
use crate::{
    model::ExecutionPlan,
    paths::Paths,
    process,
    protocol::{Control, ExecutionEvent, timing},
};
use shoal_core::execution::{DetachedLaunch, Terminal, exit_code, report_launch};
use shoal_protocol::protocol::ScopeMemory;

#[derive(Debug)]
pub(super) enum Outcome {
    Exited(i32),
    /// The daemon stopped the command without saving it.
    Stopped {
        code: i32,
        reason: String,
    },
    Paused {
        code: i32,
        stop: Halt,
    },
}

impl Outcome {
    /// The code the command ended with.
    pub(super) fn code(&self) -> i32 {
        match self {
            Self::Exited(code) | Self::Stopped { code, .. } | Self::Paused { code, .. } => *code,
        }
    }

    /// A command that ended by itself or by a signal, paused when `halt`
    /// names the end of the session it ran in.
    fn ended(status: ExitStatus, halt: Option<Halt>) -> Self {
        let code = exit_code(status);
        match halt {
            Some(stop) => Self::Paused { code, stop },
            None => Self::Exited(code),
        }
    }

    /// A command stopped with `code` because the daemon sent `control`,
    /// refused its reattachment or went away.
    fn stopped_by(control: Result<Control>, code: i32) -> Result<Self> {
        Ok(match control {
            Ok(Control::OverloadStop {
                recover,
                reason,
                resumes_when,
            }) => Self::Paused {
                code,
                stop: Halt::Protection {
                    recover,
                    reason,
                    resumes_when,
                },
            },
            Ok(Control::Pause {
                reason,
                shutdown: true,
                ..
            }) => Self::Paused {
                code,
                stop: Halt::Shutdown { reason },
            },
            Ok(Control::Pause {
                reason, removed, ..
            }) => Self::Paused {
                code,
                stop: Halt::Pause { reason, removed },
            },
            // A refused reattachment saves the session as a stop does.
            Err(error) if error.is::<Refused>() => Self::Paused {
                code,
                stop: Halt::Pause {
                    reason: Some(error.to_string()),
                    removed: false,
                },
            },
            Ok(Control::Stop {
                reason: Some(reason),
                ..
            }) => Self::Stopped { code, reason },
            Ok(Control::Stop { reason: None, .. }) => Self::Exited(code),
            Ok(_) => bail!("unexpected execution control"),
            Err(error) => {
                return Err(error.context(
                    "daemon disconnected; command stopped, execution requires reconciliation",
                ));
            }
        })
    }
}

/// How a supervised command is restored from a saved record and saved for a
/// later restore.
pub(super) struct Saving<'a, F> {
    /// The saved record a restored agent consumes once it is registered.
    pub(crate) restored_from: Option<&'a Path>,
    /// Saves what restores the command before a stop that may end this
    /// wrapper first, a signal ending its session or a system shutdown,
    /// stops it.
    pub(crate) ending: F,
}

/// Launch the command, register its process group, and wait for it to exit or
/// for a stop request, with what its scope recorded of its memory where it
/// ran in one of its own.
pub(super) async fn supervise(
    link: &mut Link,
    paths: &Paths,
    plan: &ExecutionPlan,
    command: &[OsString],
    mode: &Mode,
    auth: Option<&AgentLaunch>,
    saving: Saving<'_, impl Fn(&Halt)>,
) -> Result<(Outcome, Option<ScopeMemory>)> {
    let signals = ExecutionSignals::new()?;
    let launcher = launcher(mode).await;
    // Before the launch, so a kill before its scope is found still shows.
    let baseline = (!launcher.is_empty()).then(scope_memory::Baseline::read);
    let (mut child, group, _terminal, _scope) =
        spawn_registered_child(paths, plan, command, mode, auth, launcher, link.status())?;
    let watch = baseline.map(|baseline| baseline.watch(group.pid()));
    register_child(link, plan, mode, &group, saving.restored_from).await?;
    let report = SignalReport { paths, plan };
    let outcome = wait_for_child(link, &mut child, &group, signals, saving.ending, report).await?;
    if let Outcome::Exited(code) = outcome {
        link.status().agent_exited(code);
    }
    // A command owns its process group; descendants do not outlive its lease.
    drop(group);
    let memory = watch.and_then(|watch| watch.finish(outcome.code()));
    Ok((outcome, memory))
}

/// The command the child of `mode` starts through: Shoal's work runs in
/// the work slice, where a scope can start.
async fn launcher(mode: &Mode) -> &'static [OsString] {
    match mode.is_work() {
        true => shoal_core::service::work::launcher().await,
        false => &[],
    }
}

/// Register the process group of the child `mode` started with the daemon.
async fn register_child(
    link: &mut Link,
    plan: &ExecutionPlan,
    mode: &Mode,
    group: &ProcessGroup,
    recovery_record: Option<&Path>,
) -> Result<()> {
    let restored = matches!(mode, Mode::Recovery { .. }) || recovery_record.is_some();
    let records = restored_records(mode, recovery_record);
    register_process(link, group, restored, &records).await?;
    consume_recovery_records(&records)?;
    report_detached_launch(plan, mode, group)
}

/// Spawn a child as the terminal's foreground job.
fn spawn_registered_child(
    paths: &Paths,
    plan: &ExecutionPlan,
    command: &[OsString],
    mode: &Mode,
    auth: Option<&AgentLaunch>,
    launcher: &[OsString],
    status: &Reporter,
) -> Result<(
    Child,
    ProcessGroup,
    Option<Terminal>,
    process::scoped::Records,
)> {
    // Capture before spawning, and restore after child/process-group cleanup,
    // including failures before the terminal handoff.
    let mut terminal = Terminal::capture(true)?;
    // An agent's terminal is asked again for each child; a launch without
    // one reports nothing.
    if terminal.is_some() && auth.is_some_and(|auth| auth.reads_terminal) {
        status.query();
    }
    // Descendants that clear their scope token find the wrapper's record from
    // their first instruction, and the child's once the wrapper is gone.
    let mut scope = process::scoped::Records::wrapper(&paths.state)?;
    let child = spawn(
        paths,
        plan,
        command,
        mode,
        auth,
        terminal.as_ref(),
        launcher,
    )?;
    let group = ProcessGroup(child.id().context("child PID unavailable")? as i32);
    scope.command(group.pid())?;
    handoff_terminal(&mut terminal, &group)?;
    Ok((child, group, terminal, scope))
}

async fn register_process(
    link: &mut Link,
    group: &ProcessGroup,
    restored: bool,
    records: &[&Path],
) -> Result<()> {
    let identity = process::identity::capture(group.pid())?;
    link.send(&ExecutionEvent::Started {
        child: identity.clone(),
        group_id: group.pid(),
        restored,
        restored_from: records.iter().map(|record| record.to_path_buf()).collect(),
        reports_status: link.status().reports(),
    })
    .await?;
    let acknowledged = timeout(timing::START_ACK_TIMEOUT, link.control()).await??;
    ensure!(
        matches!(acknowledged, Control::Started),
        "daemon did not acknowledge process registration"
    );
    link.arm(identity, group.pid());
    Ok(())
}

fn report_detached_launch(plan: &ExecutionPlan, mode: &Mode, group: &ProcessGroup) -> Result<()> {
    if let Mode::Detached { log } = mode {
        report_launch(&DetachedLaunch {
            execution_id: plan.id.clone(),
            pid: group.pid(),
            log: log.clone(),
        })?;
    }
    Ok(())
}

/// Give the terminal to the child's group, which took it as it started
/// unless its standard input is another file, and continue a child stopped
/// on terminal I/O before then.
fn handoff_terminal(terminal: &mut Option<Terminal>, group: &ProcessGroup) -> Result<()> {
    if let Some(terminal) = terminal {
        terminal.give_to(group.0)?;
    }
    group.send(libc::SIGCONT);
    Ok(())
}

/// The saved records the child of `mode` restores from: a restore's, and
/// the `recovery_record` an agent resumed in place restores from.
fn restored_records<'a>(mode: &'a Mode, recovery_record: Option<&'a Path>) -> Vec<&'a Path> {
    let records = match mode {
        Mode::Recovery { records, .. } => records.as_slice(),
        _ => &[],
    };
    (records.iter().map(PathBuf::as_path))
        .chain(recovery_record)
        .collect()
}

fn consume_recovery_records(records: &[&Path]) -> Result<()> {
    for record in records {
        recovery::consume(record)?;
    }
    Ok(())
}

/// Wait for the child, daemon controls, or a signal and classify the stop;
/// `report` tells the daemon of a signal ending the session.
async fn wait_for_child(
    link: &mut Link,
    child: &mut Child,
    group: &ProcessGroup,
    mut signals: ExecutionSignals,
    ending: impl Fn(&Halt),
    report: SignalReport<'_>,
) -> Result<Outcome> {
    // While the daemon is away the link reattaches in the background and the
    // command keeps the terminal; nothing is written to it.
    loop {
        tokio::select! {
            // A hangup reaches the command and this wrapper together, so the
            // wrapper's signal goes first when both are ready.
            biased;
            _ = signals.hangup.recv() => return end_session(child, group, libc::SIGHUP, &ending, &report).await,
            _ = signals.terminate.recv() => return end_session(child, group, libc::SIGTERM, &ending, &report).await,
            _ = signals.interrupt.recv() => return Ok(Outcome::ended(stop(child, group, libc::SIGINT).await?, None)),
            _ = signals.quit.recv() => return Ok(Outcome::ended(stop(child, group, libc::SIGQUIT).await?, None)),
            _ = signals.cancelled.wait_for(|cancelled| *cancelled) => {
                return Ok(Outcome::ended(stop(child, group, libc::SIGTERM).await?, None));
            }
            status = child.wait() => {
                let status = status?;
                return Ok(Outcome::ended(status, status.signal().and_then(Halt::ended_by)));
            }
            received = link.recv() => {
                let control = match received {
                    Ok(Received::Detached | Received::Reattached) => continue,
                    Ok(Received::Control(control)) => Ok(control),
                    Err(error) => Err(error),
                };
                if let Ok(Control::OverloadStop { reason, .. }) = &control {
                    eprintln!("shoal: stopping agent: {reason}; workspace and resource leases retained");
                }
                if let Ok(Control::Pause { reason, shutdown: true, .. }) = &control {
                    ending(&Halt::Shutdown { reason: reason.clone() });
                }
                let status = stop(child, group, libc::SIGTERM).await?;
                return Outcome::stopped_by(control, exit_code(status));
            }
        }
    }
}

/// Stop the command whose session `signal` ended, after `ending` saved what
/// restores it, since the session's end, such as a shutdown, may kill this
/// wrapper before the command exits, and `report` told the daemon.
async fn end_session(
    child: &mut Child,
    group: &ProcessGroup,
    signal: i32,
    ending: impl Fn(&Halt),
    report: &SignalReport<'_>,
) -> Result<Outcome> {
    let halt = Halt::ended_by(signal);
    if let Some(halt) = &halt {
        ending(halt);
    }
    report.send(signal).await;
    let status = stop(child, group, signal).await?;
    Ok(Outcome::ended(status, halt))
}

struct ExecutionSignals {
    hangup: Signal,
    terminate: Signal,
    interrupt: Signal,
    quit: Signal,
    cancelled: watch::Receiver<bool>,
}

impl ExecutionSignals {
    fn new() -> Result<Self> {
        Ok(Self {
            hangup: signal(SignalKind::hangup())?,
            terminate: signal(SignalKind::terminate())?,
            interrupt: signal(SignalKind::interrupt())?,
            quit: signal(SignalKind::quit())?,
            cancelled: super::cancellation(),
        })
    }
}

/// The command's process group; killed when dropped.
struct ProcessGroup(i32);

impl ProcessGroup {
    fn pid(&self) -> u32 {
        self.0 as u32
    }

    async fn wait_for_exit(&self) -> Result<()> {
        process::identity::group_exited(self.pid()).await
    }

    fn send(&self, signal: i32) {
        // SAFETY: this is the process group of the child just spawned here.
        unsafe {
            libc::kill(-self.0, signal);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.send(libc::SIGKILL);
    }
}

async fn stop(child: &mut Child, group: &ProcessGroup, signal: i32) -> Result<ExitStatus> {
    group.send(signal);
    let graceful_exit = async {
        let status = child.wait().await?;
        group.wait_for_exit().await?;
        Ok::<_, anyhow::Error>(status)
    };
    match timeout(timing::EXECUTION_STOP_GRACE, graceful_exit).await {
        Ok(Ok(status)) => Ok(status),
        // Inventory failure must still force cleanup and preserve the child's
        // status; the daemon independently verifies execution completion.
        Ok(Err(_)) | Err(_) => {
            group.send(libc::SIGKILL);
            Ok(child.wait().await?)
        }
    }
}

#[cfg(test)]
mod tests;
