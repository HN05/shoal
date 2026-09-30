//! Tracked execution wrapper. The CLI process launches the command inside the
//! workspace, hands it the terminal, forwards daemon stop requests, and reports
//! completion. The daemon never touches terminal I/O. A detached launch runs
//! the same wrapper in a background `shoal` process with a log file instead
//! of the terminal, so the invoking CLI returns once the launch is recorded.
pub mod recovery;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    io::{IsTerminal, Write},
    os::unix::{fs::OpenOptionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::UnixStream,
    process::{Child, Command},
    signal::unix::{SignalKind, signal},
    time::timeout,
};

use crate::{
    cli::{
        client,
        internal::{InternalCommand, internal_command},
    },
    daemon::workspace::ExecutionKind,
    env,
    model::{ExecutionPlan, Workspace},
    paths::Paths,
    process,
    protocol::{self, Control, ExecutionEvent, Method, timing},
};

/// What a detached wrapper reports on stdout once the daemon has recorded the
/// command's process group; the CLI that spawned it returns after reading it.
#[derive(Debug, Serialize, Deserialize)]
pub struct DetachedLaunch {
    pub execution_id: String,
    pub pid: u32,
    pub log: PathBuf,
}

/// Exit status as a shell would report it: the code, or 128 + signal.
pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}

#[derive(Clone)]
enum Mode {
    /// An arbitrary command chosen by the caller.
    Command,
    Recovery {
        record: PathBuf,
    },
    /// A caller-chosen command without a terminal: no stdin, output appended
    /// to `log`, and the launch reported on stdout for the spawning CLI.
    Detached {
        log: PathBuf,
    },
    Land {
        json: bool,
    },
    /// The repository's configured setup command; `json` keeps stdout clean.
    Setup {
        json: bool,
    },
}

impl Mode {
    fn kind(&self) -> ExecutionKind {
        match self {
            Self::Command | Self::Recovery { .. } | Self::Detached { .. } => ExecutionKind::Command,
            Self::Land { .. } => ExecutionKind::Land,
            Self::Setup { .. } => ExecutionKind::Setup,
        }
    }

    fn is_setup(&self) -> bool {
        matches!(self, Mode::Setup { .. })
    }
}

/// `agent` names a Shoal agent shortcut; the daemon tells the user when it exits.
pub async fn run(
    paths: &Paths,
    workspace: String,
    command: Vec<OsString>,
    agent: Option<String>,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a command is required after --");
    run_tracked(paths, workspace, command, Mode::Command, agent).await
}

/// Consume the selected recovery record only after the replacement process is
/// recorded, so failed launches remain recoverable without duplicating sessions.
pub async fn run_recovery(
    paths: &Paths,
    workspace: String,
    command: Vec<OsString>,
    agent: String,
    record: PathBuf,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a restore command is required");
    run_tracked(
        paths,
        workspace,
        command,
        Mode::Recovery { record },
        Some(agent),
    )
    .await
}

/// The background half of a detached launch: an ordinary tracked wrapper whose
/// child reads `/dev/null` and writes `log`. It stays alive as the execution's
/// wrapper, so stop requests, removal, and recovery see a connected command.
pub async fn run_detached_wrapper(
    paths: &Paths,
    workspace: String,
    log: PathBuf,
    command: Vec<OsString>,
    agent: Option<String>,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a command is required after --");
    run_tracked(paths, workspace, command, Mode::Detached { log }, agent).await
}

/// Start `command` in `workspace` as a tracked execution that outlives this
/// process: a new session running this binary's detached wrapper, with the
/// command's output in `log`, the inherited `clear` variables removed and
/// `env` added to its environment, and `agent` naming the shortcut for the
/// exit notification. Returns once the daemon has recorded the launch.
pub async fn launch_detached(
    paths: &Paths,
    workspace: &Workspace,
    log: PathBuf,
    command: Vec<OsString>,
    clear: &[&str],
    env: &[(&str, String)],
    agent: Option<&str>,
) -> Result<DetachedLaunch> {
    ensure!(workspace.path.is_dir(), "workspace directory is missing");
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("open {}", log.display()))?;
    writeln!(
        file,
        "shoal: starting {} in {}",
        command
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        workspace.path.display()
    )?;
    let command = internal_command(
        paths,
        false,
        InternalCommand::Detached {
            workspace: &workspace.id,
            log: &log,
            agent,
            command: &command,
        },
    )?;
    let mut wrapper = Command::new(&command[0]);
    wrapper
        .args(&command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(file))
        .env_remove(env::SHELL_DIRECTIVE);
    // A launch from inside another launcher's session must not inherit its
    // attachment; the caller names what to drop and what to set.
    for name in clear {
        wrapper.env_remove(name);
    }
    wrapper.envs(env.iter().map(|(name, value)| (*name, value)));
    // SAFETY: setsid only detaches the child from this terminal session and
    // runs before exec in the forked child, without allocating.
    unsafe {
        wrapper.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = wrapper
        .spawn()
        .context("launch detached execution wrapper")?;
    let stdout = child.stdout.take().context("wrapper stdout unavailable")?;
    let report = timeout(
        timing::DETACHED_LAUNCH_TIMEOUT,
        BufReader::new(stdout).lines().next_line(),
    )
    .await
    .context("detached launch was not recorded in time")??;
    match report {
        Some(line) => serde_json::from_str(&line).context("unexpected detached launch report"),
        None => {
            let status = match timeout(timing::DETACHED_EXIT_TIMEOUT, child.wait()).await {
                Ok(Ok(status)) => format!("wrapper exited with {status}"),
                _ => "wrapper still running".into(),
            };
            bail!(
                "detached launch failed ({status}); see {}\n{}",
                log.display(),
                log_tail(&log)
            )
        }
    }
}

/// The last lines of a log, for an error message.
fn log_tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<_> = text.lines().rev().take(10).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

pub async fn land(paths: &Paths, workspace: String, json: bool) -> Result<i32> {
    run_tracked(paths, workspace, vec![], Mode::Land { json }, None).await
}

pub async fn setup(paths: &Paths, workspace: String, json: bool) -> Result<i32> {
    run_tracked(paths, workspace, vec![], Mode::Setup { json }, None).await
}

async fn run_tracked(
    paths: &Paths,
    workspace: String,
    command: Vec<OsString>,
    mode: Mode,
    agent: Option<String>,
) -> Result<i32> {
    let auth = if agent.is_some() {
        client::settings(
            paths,
            crate::protocol::ConfigTarget::Workspace(workspace.clone()),
        )
        .await?
        .agent_auth
        .prepare(paths)?
    } else {
        None
    };
    let wrapper = process::identity::capture(std::process::id())?
        .context("cannot identify execution wrapper")?;
    let kind = mode.kind();
    let recovery = if let Some(agent) = &agent {
        Some(recovery::Recovery::for_launch(paths, &workspace, agent).await)
    } else {
        None
    };
    let method = Method::Execute {
        workspace,
        wrapper,
        kind,
        agent,
        recover: recovery.as_ref().is_some_and(|recovery| recovery.automatic),
    };
    let (mut stream, body) = timeout(kind.start_timeout(), client::open(paths, method))
        .await
        .context("daemon did not start the execution in time")??;
    let mut plan = ExecutionPlan::try_from(body)?;
    let command = if let Some(land) = &plan.land {
        internal_command(
            paths,
            matches!(mode, Mode::Land { json: true }),
            InternalCommand::Land {
                plan: &serde_json::to_string(land)?,
            },
        )?
    } else {
        match &plan.setup_cmd {
            Some(path) => vec![path.as_os_str().to_owned()],
            None => command,
        }
    };
    let mut next_command = command;
    let mut recovery_record = None;
    let mut result = loop {
        let outcome = if mode.is_setup() && next_command.is_empty() {
            Ok(Outcome::Exited(0))
        } else {
            supervise(
                &mut stream,
                paths,
                &plan,
                &next_command,
                &mode,
                auth.as_ref(),
                recovery_record.as_deref(),
            )
            .await
        };
        match outcome {
            Ok(Outcome::Exited(code)) => break Ok(code),
            Ok(Outcome::Overloaded { code, recover }) => {
                if let Some(recovery) = &recovery {
                    match recovery.save(paths, &plan.workspace.id, &plan.id) {
                        Ok(path) => recovery_record = Some(path),
                        Err(error) => eprintln!("warning: cannot save recovery command: {error:#}"),
                    }
                    eprintln!(
                        "shoal: agent stopped for overload; restore with shoal resume {} --execution {}",
                        plan.workspace.name, plan.id
                    );
                    if recover && recovery.automatic {
                        protocol::write(&mut stream, &ExecutionEvent::Paused).await?;
                        eprintln!("shoal: waiting for healthy load before restoring agent session");
                        if let Some(ports) = recovery::wait(&mut stream).await? {
                            plan.ports = ports;
                            next_command = recovery.command.clone();
                            eprintln!("shoal: restoring agent session");
                            continue;
                        }
                    }
                }
                break Ok(code);
            }
            Err(error) => break Err(error),
        }
    };
    if !matches!(result, Ok(0))
        && let Some(land) = &plan.land
        && let Err(error) = crate::git::repo::rollback_land(land).await
    {
        result = Err(error);
    }
    let code = result.as_ref().copied().unwrap_or(1);
    // Report only after child/process-group cleanup. A lost connection never
    // grants the daemon permission to assume processes stopped.
    let report = report_completion(&mut stream, code, &mode).await;
    if mode.is_setup() {
        report?;
    } else if let Err(error) = report {
        eprintln!("warning: unable to report execution completion: {error:#}");
    }
    result
}

enum Outcome {
    Exited(i32),
    Overloaded { code: i32, recover: bool },
}

/// Launch the command, register its process group, and wait for it to exit or
/// for a stop request.
async fn supervise(
    stream: &mut UnixStream,
    paths: &Paths,
    plan: &ExecutionPlan,
    command: &[OsString],
    mode: &Mode,
    auth: Option<&crate::agent_auth::Launch>,
    recovery_record: Option<&Path>,
) -> Result<Outcome> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut quit = signal(SignalKind::quit())?;
    // Capture before spawning, and restore after child/process-group cleanup,
    // including failures before the terminal handoff.
    let mut terminal = Terminal::capture(true)?;
    let mut child = spawn(paths, plan, command, mode, auth)?;
    let group = ProcessGroup(child.id().context("child PID unavailable")? as i32);
    protocol::write(
        stream,
        &ExecutionEvent::Started {
            child: process::identity::capture(group.pid())?,
            group_id: group.pid(),
        },
    )
    .await?;
    let acknowledged =
        timeout(timing::START_ACK_TIMEOUT, protocol::read::<Control>(stream)).await??;
    ensure!(
        matches!(acknowledged, Control::Started),
        "daemon did not acknowledge process registration"
    );
    if let Mode::Recovery { record } = mode {
        recovery::consume(record)?;
    }
    if let Some(record) = recovery_record {
        recovery::consume(record)?;
    }
    if let Mode::Detached { log } = mode {
        report_launch(&DetachedLaunch {
            execution_id: plan.id.clone(),
            pid: group.pid(),
            log: log.clone(),
        })?;
    }
    if let Some(terminal) = &mut terminal {
        terminal.give_to(group.0)?;
    }
    // The child may have been stopped by SIGTTIN/SIGTTOU before it became the
    // foreground group.
    group.send(libc::SIGCONT);
    let control = protocol::read::<Control>(stream);
    tokio::pin!(control);
    let mut overload = None;
    let status = tokio::select! {
        status = child.wait() => status?,
        result = &mut control => {
            let status = stop(&mut child, &group, libc::SIGTERM).await?;
            let control = result.context("daemon disconnected; command stopped, execution requires reconciliation")?;
            match control {
                Control::OverloadStop { recover } => overload = Some(recover),
                Control::Stop => {},
                _ => bail!("unexpected execution control"),
            }
            status
        }
        _ = terminate.recv() => stop(&mut child, &group, libc::SIGTERM).await?,
        _ = interrupt.recv() => stop(&mut child, &group, libc::SIGINT).await?,
        _ = quit.recv() => stop(&mut child, &group, libc::SIGQUIT).await?,
    };
    // A command owns its process group; descendants do not outlive its lease.
    drop(group);
    Ok(match overload {
        Some(recover) => Outcome::Overloaded {
            code: exit_code(status),
            recover,
        },
        None => Outcome::Exited(exit_code(status)),
    })
}

fn spawn(
    paths: &Paths,
    plan: &ExecutionPlan,
    command: &[OsString],
    mode: &Mode,
    auth: Option<&crate::agent_auth::Launch>,
) -> Result<Child> {
    let mut process = Command::new(&command[0]);
    process
        .args(&command[1..])
        .current_dir(&plan.workspace.path);
    configure_environment(&mut process, paths, plan);
    if let Some(auth) = auth {
        process.env("PATH", &auth.path);
    }
    let quiet = matches!(mode, Mode::Setup { json: true });
    let (stdin, stdout, stderr) = match mode {
        Mode::Detached { log } => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
                .with_context(|| format!("open {}", log.display()))?;
            let copy = file.try_clone()?;
            (Stdio::null(), Stdio::from(file), Stdio::from(copy))
        }
        _ if quiet => (
            Stdio::null(),
            Stdio::from(std::io::stderr()),
            Stdio::inherit(),
        ),
        _ => (Stdio::inherit(), Stdio::inherit(), Stdio::inherit()),
    };
    process
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("launch workspace command")
}

/// Tell the spawning CLI that the launch is recorded, then point stdout at the
/// log (the wrapper's stderr) so nothing else ever writes to the closed pipe.
fn report_launch(launch: &DetachedLaunch) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, launch)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    // SAFETY: duplicating one open descriptor onto another in this process.
    ensure!(
        unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } != -1,
        "redirect wrapper output to the log"
    );
    Ok(())
}

/// Export the execution's identity and port reservations, dropping stale port
/// variables inherited from an enclosing execution.
fn configure_environment(process: &mut Command, paths: &Paths, plan: &ExecutionPlan) {
    env::apply_workspace_identity(process, &plan.workspace, paths);
    let exported: Vec<_> = plan.ports.iter().map(|p| p.env_var.as_str()).collect();
    process
        .env(env::SCOPE_TOKEN, &plan.scope_token)
        .env(env::EXECUTION_ID, &plan.id)
        .envs(plan.ports.iter().map(|p| (&p.env_var, p.port.to_string())))
        .env(env::RESERVED_PORT_ENV, exported.join(":"));
}

async fn report_completion(stream: &mut UnixStream, code: i32, mode: &Mode) -> Result<bool> {
    let exchange = async {
        protocol::write(stream, &ExecutionEvent::Finished { exit_code: code }).await?;
        loop {
            if let Control::Finished { complete } = protocol::read::<Control>(stream).await? {
                if !complete {
                    eprintln!(
                        "warning: execution has surviving or unverified processes; run shoal doctor to inspect it"
                    );
                    ensure!(
                        !mode.is_setup(),
                        "setup has surviving or unverified processes"
                    );
                }
                return Ok(complete);
            }
        }
    };
    timeout(timing::COMPLETION_ACK_TIMEOUT, exchange)
        .await
        .context("execution completion acknowledgement timed out")?
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
        while !process::identity::related(None, Some(self.pid()))
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

/// Restore foreground ownership, OS settings and shell input modes on exit.
pub(crate) struct Terminal {
    previous_group: i32,
    previous_settings: libc::termios,
    previous_handler: Option<libc::sighandler_t>,
    output: Option<std::fs::File>,
}

impl Terminal {
    pub(crate) fn stdin_is_background() -> bool {
        std::io::stdin().is_terminal()
            && unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) != libc::getpgrp() }
    }

    /// Capture before launching a child. Background hooks skip the terminal;
    /// tracked commands require foreground ownership when stdin is a terminal.
    pub(crate) fn capture(require_foreground: bool) -> Result<Option<Self>> {
        if !std::io::stdin().is_terminal() {
            return Ok(None);
        }
        // SAFETY: tcgetpgrp/getpgrp inspect the calling process and stdin.
        let previous_group = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        if previous_group != unsafe { libc::getpgrp() } {
            ensure!(
                !require_foreground,
                "cannot hand the terminal to a command unless shoal is a foreground terminal job"
            );
            return Ok(None);
        }
        let mut settings = std::mem::MaybeUninit::uninit();
        // SAFETY: tcgetattr initializes settings on success.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, settings.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error()).context("capture terminal settings");
        }
        Ok(Some(Self {
            previous_group,
            previous_settings: unsafe { settings.assume_init() },
            previous_handler: None,
            output: Self::input_mode_output(),
        }))
    }

    pub(crate) fn give_to(&mut self, group: i32) -> Result<()> {
        // Ignore SIGTTOU only after spawning, so the child keeps normal job
        // control while the wrapper can reclaim the terminal from background.
        self.previous_handler = Some(unsafe { libc::signal(libc::SIGTTOU, libc::SIG_IGN) });
        // SAFETY: stdin is our foreground terminal, captured before launch.
        ensure!(
            unsafe { libc::tcsetpgrp(libc::STDIN_FILENO, group) } == 0,
            "cannot hand terminal to command"
        );
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: ignore SIGTTOU while reclaiming our terminal from the child's
        // foreground group. TCSANOW restores settings without discarding input
        // or waiting indefinitely for output to drain.
        unsafe {
            let previous_handler = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            libc::tcsetpgrp(libc::STDIN_FILENO, self.previous_group);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.previous_settings);
            if self.previous_handler.is_some() {
                self.reset_input_modes();
            }
            libc::signal(
                libc::SIGTTOU,
                self.previous_handler.unwrap_or(previous_handler),
            );
        }
    }
}

impl Terminal {
    fn input_mode_output() -> Option<std::fs::File> {
        let term = std::env::var_os("TERM")?;
        if term.is_empty() || term == "dumb" {
            return None;
        }
        // Use the controlling terminal even when both output streams are
        // redirected. Cleanup must neither enter a pipe nor block on output,
        // and inability to open the output must not prevent command launch.
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open("/dev/tty")
            .ok()
    }

    fn reset_input_modes(&mut self) {
        let Some(output) = &mut self.output else {
            return;
        };
        // termios cannot undo emulator modes left by an interrupted TUI.
        // Disable mouse tracking/encodings, focus and paste reports, and restore
        // ordinary cursor/keypad and keyboard input. Avoid a full terminal reset,
        // which would clear the user's scrollback and other terminal settings.
        let _ = output.write_all(
            concat!(
                "\x1b[?1000l\x1b[?1002l\x1b[?1003l",
                "\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1016l",
                "\x1b[?1004l\x1b[?1007l\x1b[?2004l",
                "\x1b[?1l\x1b>\x1b[>4;0m\x1b[=0u",
            )
            .as_bytes(),
        );
    }
}
