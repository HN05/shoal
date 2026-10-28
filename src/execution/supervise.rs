//! Supervising a tracked command: registering its process group with the
//! daemon, handing it the terminal, and waiting for it to exit or be stopped.
use std::{
    ffi::OsString, os::unix::process::ExitStatusExt, path::Path, process::ExitStatus,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use tokio::{
    process::Child,
    signal::unix::{Signal, SignalKind, signal},
    time::timeout,
};

use super::{
    Mode,
    detached::{DetachedLaunch, report_launch},
    link::{Link, Received, Refused},
    pause::Halt,
    recovery,
    spawn::{AgentLaunch, spawn},
    terminal::Terminal,
};
use crate::{
    model::ExecutionPlan,
    paths::Paths,
    process,
    protocol::{Control, ExecutionEvent, timing},
};

/// Exit status as a shell would report it: the code, or 128 + signal.
pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}

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
            Ok(Control::Pause { reason, removed }) => Self::Paused {
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
            }) => Self::Stopped { code, reason },
            Ok(Control::Stop { reason: None }) => Self::Exited(code),
            Ok(_) => bail!("unexpected execution control"),
            Err(error) => {
                return Err(error.context(
                    "daemon disconnected; command stopped, execution requires reconciliation",
                ));
            }
        })
    }
}

/// Launch the command, register its process group, and wait for it to exit or
/// for a stop request.
pub(super) async fn supervise(
    link: &mut Link,
    paths: &Paths,
    plan: &ExecutionPlan,
    command: &[OsString],
    mode: &Mode,
    auth: Option<&AgentLaunch>,
    recovery_record: Option<&Path>,
) -> Result<Outcome> {
    let signals = ExecutionSignals::new()?;
    let (mut child, group, _terminal, _scope) =
        register_child(link, paths, plan, command, mode, auth, recovery_record).await?;
    let outcome = wait_for_child(link, &mut child, &group, signals).await?;
    // A command owns its process group; descendants do not outlive its lease.
    drop(group);
    Ok(outcome)
}

/// Spawn a child, register its process group with the daemon, and hand over
/// the terminal only after registration is acknowledged.
async fn register_child(
    link: &mut Link,
    paths: &Paths,
    plan: &ExecutionPlan,
    command: &[OsString],
    mode: &Mode,
    auth: Option<&AgentLaunch>,
    recovery_record: Option<&Path>,
) -> Result<(
    Child,
    ProcessGroup,
    Option<Terminal>,
    process::scoped::Records,
)> {
    let (child, group, mut terminal, scope) =
        spawn_registered_child(paths, plan, command, mode, auth)?;
    register_process(link, &group).await?;
    consume_recovery_records(mode, recovery_record)?;
    report_detached_launch(plan, mode, &group)?;
    handoff_terminal(&mut terminal, &group)?;
    // The child may have been stopped by SIGTTIN/SIGTTOU before it became the
    // foreground group.
    group.send(libc::SIGCONT);
    Ok((child, group, terminal, scope))
}

fn spawn_registered_child(
    paths: &Paths,
    plan: &ExecutionPlan,
    command: &[OsString],
    mode: &Mode,
    auth: Option<&AgentLaunch>,
) -> Result<(
    Child,
    ProcessGroup,
    Option<Terminal>,
    process::scoped::Records,
)> {
    // Capture before spawning, and restore after child/process-group cleanup,
    // including failures before the terminal handoff.
    let terminal = Terminal::capture(true)?;
    // Descendants that clear their scope token find the wrapper's record from
    // their first instruction, and the child's once the wrapper is gone.
    let mut scope = process::scoped::Records::wrapper(&paths.state)?;
    let child = spawn(paths, plan, command, mode, auth)?;
    let group = ProcessGroup(child.id().context("child PID unavailable")? as i32);
    scope.command(group.pid())?;
    Ok((child, group, terminal, scope))
}

async fn register_process(link: &mut Link, group: &ProcessGroup) -> Result<()> {
    let identity = process::identity::capture(group.pid())?;
    link.send(&ExecutionEvent::Started {
        child: identity.clone(),
        group_id: group.pid(),
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

fn handoff_terminal(terminal: &mut Option<Terminal>, group: &ProcessGroup) -> Result<()> {
    if let Some(terminal) = terminal {
        terminal.give_to(group.0)?;
    }
    Ok(())
}

fn consume_recovery_records(mode: &Mode, recovery_record: Option<&Path>) -> Result<()> {
    if let Mode::Recovery { records } = mode {
        for record in records {
            recovery::consume(record)?;
        }
    }
    if let Some(record) = recovery_record {
        recovery::consume(record)?;
    }
    Ok(())
}

/// Wait for the child, daemon controls, or a signal and classify the stop.
async fn wait_for_child(
    link: &mut Link,
    child: &mut Child,
    group: &ProcessGroup,
    mut signals: ExecutionSignals,
) -> Result<Outcome> {
    // While the daemon is away the link reattaches in the background and the
    // command keeps the terminal; nothing is written to it.
    loop {
        tokio::select! {
            // A hangup reaches the command and this wrapper together, so the
            // wrapper's signal goes first when both are ready.
            biased;
            _ = signals.hangup.recv() => {
                let status = stop(child, group, libc::SIGHUP).await?;
                return Ok(Outcome::ended(status, Halt::ended_by(libc::SIGHUP)));
            }
            _ = signals.terminate.recv() => {
                let status = stop(child, group, libc::SIGTERM).await?;
                return Ok(Outcome::ended(status, Halt::ended_by(libc::SIGTERM)));
            }
            _ = signals.interrupt.recv() => return Ok(Outcome::ended(stop(child, group, libc::SIGINT).await?, None)),
            _ = signals.quit.recv() => return Ok(Outcome::ended(stop(child, group, libc::SIGQUIT).await?, None)),
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
                let status = stop(child, group, libc::SIGTERM).await?;
                return Outcome::stopped_by(control, exit_code(status));
            }
        }
    }
}

struct ExecutionSignals {
    hangup: Signal,
    terminate: Signal,
    interrupt: Signal,
    quit: Signal,
}

impl ExecutionSignals {
    fn new() -> Result<Self> {
        Ok(Self {
            hangup: signal(SignalKind::hangup())?,
            terminate: signal(SignalKind::terminate())?,
            interrupt: signal(SignalKind::interrupt())?,
            quit: signal(SignalKind::quit())?,
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
        // The leader may exit before descendants finish their signal handlers.
        // Native identity checks exclude zombies that can no longer clean up.
        while !process::identity::related(None, Some(self.pid()), None)
            .await?
            .1
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Ok(())
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
