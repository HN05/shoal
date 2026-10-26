//! Tracked execution wrapper. The CLI process launches the command inside the
//! workspace, hands it the terminal, forwards daemon stop requests, and reports
//! completion. The daemon never touches terminal I/O. A detached launch runs
//! the same wrapper in a background `shoal` process with a log file instead
//! of the terminal, so the invoking CLI returns once the launch is recorded.
mod completion;
mod demo;
mod detached;
mod link;
mod pause;
pub mod recovery;
mod terminal;
mod wake;

use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsString,
    io::{IsTerminal, Write},
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    process::{Child, Command},
    signal::unix::{Signal, SignalKind, signal},
    time::timeout,
};

use crate::{
    cli::{
        client,
        internal::{Worker, internal_command},
    },
    daemon::{approvals::Undecided, workspace::ExecutionKind},
    env,
    model::ExecutionPlan,
    paths::Paths,
    process,
    protocol::{Body, Control, ExecutionEvent, Method, Reattach, timing},
};
pub use completion::SetupVerificationFailure;
use completion::report_completion;
#[cfg(feature = "happy")]
pub use detached::launch_detached;
use detached::{DetachedLaunch, report_launch};
use link::{Link, Received, Refused};
use pause::{AfterPause, Halt, pause_agent, record_stopped_command};
pub(crate) use terminal::{Terminal, reset_input_modes, run_in_terminal};

/// Exit status as a shell would report it: the code, or 128 + signal.
pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}

#[derive(Clone)]
enum Mode {
    /// An arbitrary command chosen by the caller; `record` keeps it for
    /// `shoal resume` to report when `shoal stop` interrupts it.
    Command {
        record: bool,
    },
    /// A restored agent session; `records` are consumed once it starts.
    Recovery {
        records: Vec<PathBuf>,
    },
    /// A caller-chosen command without a terminal: no stdin, output appended
    /// to `log`, and the launch reported on stdout for the spawning CLI.
    Detached {
        log: PathBuf,
    },
    Land {
        json: bool,
        push: bool,
    },
    /// A command publishing for the workspace under its agent account, with
    /// output on stderr so stdout stays for the caller's result.
    Publish,
    /// The repository's configured setup command; `json` keeps stdout clean.
    Setup {
        json: bool,
    },
    /// A demo, restarted whenever its command exits until it is stopped.
    Demo {
        launch: Box<crate::demos::Launch>,
    },
}

impl Mode {
    fn kind(&self) -> ExecutionKind {
        match self {
            Self::Command { .. }
            | Self::Recovery { .. }
            | Self::Detached { .. }
            | Self::Publish
            | Self::Demo { .. } => ExecutionKind::Command,
            Self::Land { .. } => ExecutionKind::Land,
            Self::Setup { .. } => ExecutionKind::Setup,
        }
    }

    fn is_setup(&self) -> bool {
        matches!(self, Mode::Setup { .. })
    }

    /// Commands keep running across a daemon restart; setup and landing
    /// depend on gates the daemon holds.
    fn reattaches(&self) -> bool {
        self.kind() == ExecutionKind::Command
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
    let ran_agent = agent.is_some();
    let result = run_tracked(
        paths,
        workspace,
        command,
        Mode::Command { record: false },
        agent,
    )
    .await;
    if ran_agent {
        return result.map(env::session_exit_code);
    }
    result
}

/// A command the user chose, recorded when `shoal stop` interrupts it.
pub async fn run_command(paths: &Paths, workspace: String, command: Vec<OsString>) -> Result<i32> {
    ensure!(!command.is_empty(), "a command is required after --");
    run_tracked(
        paths,
        workspace,
        command,
        Mode::Command { record: true },
        None,
    )
    .await
}

/// Consume the selected recovery records only after the replacement process is
/// recorded, so failed launches remain recoverable without duplicating sessions.
pub async fn run_recovery(
    paths: &Paths,
    workspace: String,
    command: Vec<OsString>,
    agent: String,
    records: Vec<PathBuf>,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a restore command is required");
    run_tracked(
        paths,
        workspace,
        command,
        Mode::Recovery { records },
        Some(agent),
    )
    .await
    .map(env::session_exit_code)
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

/// A demo's wrapper, which records `launch` with its execution and restarts
/// `command` until the demo is stopped; a stopped demo exits 0.
pub async fn run_demo(
    paths: &Paths,
    workspace: String,
    launch: crate::demos::Launch,
    command: Vec<OsString>,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a command is required after --");
    clear_terminal();
    let mode = Mode::Demo {
        launch: Box::new(launch),
    };
    run_tracked(paths, workspace, command, mode, None)
        .await
        .map(|_| 0)
}

/// Clear the screen and scrollback of the terminal a demo starts in, which
/// shows how its host started the wrapper, so the user sees only the demo.
fn clear_terminal() {
    let mut stdout = std::io::stdout();
    if stdout.is_terminal() {
        let _ = stdout
            .write_all(b"\x1b[H\x1b[2J\x1b[3J")
            .and_then(|()| stdout.flush());
    }
}

pub async fn publish(paths: &Paths, workspace: String, command: Vec<OsString>) -> Result<i32> {
    run_tracked(paths, workspace, command, Mode::Publish, None).await
}

pub async fn land(paths: &Paths, workspace: String, json: bool, push: bool) -> Result<i32> {
    run_tracked(paths, workspace, vec![], Mode::Land { json, push }, None).await
}

pub async fn setup(paths: &Paths, workspace: String, json: bool) -> Result<i32> {
    run_tracked(paths, workspace, vec![], Mode::Setup { json }, None).await
}

struct TrackedLaunch {
    link: Link,
    plan: ExecutionPlan,
    command: Vec<OsString>,
    auth: Option<AgentLaunch>,
    recovery: Option<recovery::Recovery>,
}

async fn run_tracked(
    paths: &Paths,
    workspace: String,
    command: Vec<OsString>,
    mode: Mode,
    agent: Option<String>,
) -> Result<i32> {
    let TrackedLaunch {
        mut link,
        mut plan,
        command,
        auth,
        recovery,
    } = prepare_tracked_launch(paths, workspace, command, &mode, agent).await?;
    let integration = auth.as_ref().and_then(|auth| auth.integration.as_deref());
    let wake = wake::session(integration, &mode);
    let mut next_command = command;
    let mut recovery_record = None;
    let mut restarts = demo::Restarts::default();
    let mut result = loop {
        let started = Instant::now();
        let outcome = if mode.is_setup() && next_command.is_empty() {
            Ok(Outcome::Exited(0))
        } else {
            // Prompts only while this child runs, which reports its state.
            let _waker = wake
                .clone()
                .map(|session| wake::start(paths, &plan.workspace.id, session));
            supervise(
                &mut link,
                paths,
                &plan,
                &next_command,
                &mode,
                auth.as_ref(),
                recovery_record.as_deref(),
            )
            .await
        };
        if let Some(refusal) = link.refusal() {
            eprintln!("shoal: {refusal}; command stopped");
        }
        match outcome {
            Ok(Outcome::Exited(code)) if matches!(mode, Mode::Demo { .. }) => {
                let delay = restarts.after(started.elapsed());
                eprintln!(
                    "shoal: demo exited with status {code}; restarting in {}s",
                    delay.as_secs()
                );
                match demo::next_run(&mut link, delay).await? {
                    None => continue,
                    Some(reason) => {
                        eprintln!("shoal: demo stopped: {reason}");
                        break Ok(code);
                    }
                }
            }
            Ok(Outcome::Exited(code)) => break Ok(code),
            Ok(Outcome::Stopped { code, reason }) => {
                let stopped = if recovery.is_some() {
                    "agent"
                } else {
                    "command"
                };
                eprintln!("shoal: {stopped} stopped: {reason}");
                break Ok(code);
            }
            // Only an agent's session is saved when its own session ends.
            Ok(Outcome::Paused {
                code,
                stop: Halt::Ended { .. },
            }) if recovery.is_none() => break Ok(code),
            Ok(Outcome::Paused { code, stop }) => {
                if let Some(recovery) = &recovery {
                    let after =
                        pause_agent(&mut link, paths, &plan, &mode, recovery, &stop).await?;
                    if let AfterPause::Restart {
                        command,
                        ports,
                        record,
                    } = after
                    {
                        plan.ports = ports;
                        next_command = command;
                        recovery_record = record;
                        continue;
                    }
                } else if matches!(mode, Mode::Command { record: true }) {
                    record_stopped_command(paths, &plan, &next_command, &stop);
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
    if link.refusal().is_some() {
        return result;
    }
    // Report only after child/process-group cleanup. A lost connection never
    // grants the daemon permission to assume processes stopped.
    let report = report_completion(&mut link, code, &mode).await;
    if mode.is_setup() {
        report?;
    } else if let Err(error) = report {
        eprintln!("warning: unable to report execution completion: {error:#}");
    }
    result
}

async fn prepare_tracked_launch(
    paths: &Paths,
    workspace: String,
    command: Vec<OsString>,
    mode: &Mode,
    agent: Option<String>,
) -> Result<TrackedLaunch> {
    let auth = prepare_agent_launch(paths, &workspace, mode, agent.as_deref()).await?;
    let recovery = if let Some(agent) = &agent {
        Some(recovery::Recovery::for_launch(paths, &workspace, agent).await)
    } else {
        None
    };
    let recover = recovery.as_ref().is_some_and(|recovery| recovery.automatic);
    let (link, plan) = open_execution(paths, workspace, mode, agent, recover).await?;
    let command = execution_command(paths, &plan, command, mode)?;
    Ok(TrackedLaunch {
        link,
        plan,
        command,
        auth,
        recovery,
    })
}

async fn open_execution(
    paths: &Paths,
    workspace: String,
    mode: &Mode,
    agent: Option<String>,
    recover: bool,
) -> Result<(Link, ExecutionPlan)> {
    let wrapper = process::identity::capture(std::process::id())?
        .context("cannot identify execution wrapper")?;
    let method = Method::Execute {
        workspace,
        wrapper: wrapper.clone(),
        kind: mode.kind(),
        queue_entry: agent
            .is_some()
            .then(crate::env::queue_entry)
            .flatten()
            .map(str::to_owned),
        agent: agent.clone(),
        recover,
        reattach: mode.reattaches(),
        demo: match mode {
            Mode::Demo { launch } => Some(launch.clone()),
            _ => None,
        },
    };
    let kind = mode.kind();
    let (mut link, body) = Link::open(paths, method, kind.start_timeout()).await?;
    let plan = match body {
        Body::ApprovalRequest(request) => {
            return Err(Undecided(request).into());
        }
        body => ExecutionPlan::try_from(body)?,
    };
    if mode.reattaches() {
        link.allow_reattach(
            paths,
            Reattach {
                execution: plan.id.clone(),
                wrapper,
                child: None,
                group_id: 0,
                scope_token: plan.scope_token.clone(),
                agent,
                recover,
                running_ms: 0,
                awaiting_restore: None,
                between_runs: false,
            },
        );
    }
    Ok((link, plan))
}

async fn prepare_agent_launch(
    paths: &Paths,
    workspace: &str,
    mode: &Mode,
    agent: Option<&str>,
) -> Result<Option<AgentLaunch>> {
    let has_agent = agent.is_some();
    if !has_agent && !matches!(mode, Mode::Publish) {
        return Ok(None);
    }
    let (config, settings) = client::configuration(
        paths,
        crate::protocol::ConfigTarget::Workspace(workspace.to_owned()),
    )
    .await?;
    let secrets = if has_agent {
        crate::secrets::Launch::prepare(&settings.secrets, paths).await?
    } else {
        Default::default()
    };
    Ok(Some(AgentLaunch {
        account: settings.agent_auth.prepare(paths, &config.git)?,
        secrets,
        output: has_agent.then(env::agent_output).flatten(),
        integration: agent.map(|agent| settings.integration(agent).to_owned()),
        environment: agent
            .map(|agent| settings.environment(agent))
            .unwrap_or_default(),
        home: paths.home.clone(),
    }))
}

fn execution_command(
    paths: &Paths,
    plan: &ExecutionPlan,
    command: Vec<OsString>,
    mode: &Mode,
) -> Result<Vec<OsString>> {
    if let Some(land) = &plan.land {
        return internal_command(
            paths,
            matches!(mode, Mode::Land { json: true, .. }),
            Worker::Land {
                plan: &serde_json::to_string(land)?,
                push: matches!(mode, Mode::Land { push: true, .. }),
            },
        );
    }
    if plan.copy_ignored {
        return internal_command(
            paths,
            false,
            Worker::CopyIgnored {
                setup_cmd: plan.setup_cmd.as_deref(),
            },
        );
    }
    Ok(match &plan.setup_cmd {
        Some(path) => vec![path.as_os_str().to_owned()],
        None => command,
    })
}

#[derive(Debug)]
enum Outcome {
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

/// What an agent's or publishing child receives beyond the workspace
/// environment: the agent account and, for agents, the configured secrets and
/// the output default.
struct AgentLaunch {
    account: crate::agent_auth::Launch,
    secrets: crate::secrets::Launch,
    output: Option<(&'static str, &'static str)>,
    /// The agent's integration, which launch profiles share with their base.
    integration: Option<String>,
    /// What the agent's `[ai]` entry changes in its environment, last.
    environment: crate::ai::Environment,
    home: PathBuf,
}

impl AgentLaunch {
    fn apply(&self, command: &mut Command) {
        self.account.apply(command);
        self.secrets.apply(command);
        command.envs(self.output);
        self.environment.apply(command.as_std_mut(), &self.home);
    }
}

/// Launch the command, register its process group, and wait for it to exit or
/// for a stop request.
async fn supervise(
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
    let (status, recovery, stopped) = wait_for_child(link, &mut child, &group, signals).await?;
    // A command owns its process group; descendants do not outlive its lease.
    drop(group);
    let code = exit_code(status);
    Ok(match (recovery, stopped) {
        (Some(stop), _) => Outcome::Paused { code, stop },
        (None, Some(reason)) => Outcome::Stopped { code, reason },
        (None, None) => Outcome::Exited(code),
    })
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
) -> Result<(ExitStatus, Option<Halt>, Option<String>)> {
    // While the daemon is away the link reattaches in the background and the
    // command keeps the terminal; nothing is written to it.
    loop {
        tokio::select! {
            // A hangup reaches the command and this wrapper together, so the
            // wrapper's signal goes first when both are ready.
            biased;
            _ = signals.hangup.recv() => {
                let status = stop(child, group, libc::SIGHUP).await?;
                return Ok((status, Halt::ended_by(libc::SIGHUP), None));
            }
            _ = signals.terminate.recv() => {
                let status = stop(child, group, libc::SIGTERM).await?;
                return Ok((status, Halt::ended_by(libc::SIGTERM), None));
            }
            _ = signals.interrupt.recv() => return Ok((stop(child, group, libc::SIGINT).await?, None, None)),
            _ = signals.quit.recv() => return Ok((stop(child, group, libc::SIGQUIT).await?, None, None)),
            status = child.wait() => {
                let status = status?;
                return Ok((status, status.signal().and_then(Halt::ended_by), None));
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
                let (recovery, stopped) = match control {
                    Ok(Control::OverloadStop {
                        recover,
                        reason,
                        resumes_when,
                    }) => (
                        Some(Halt::Protection {
                            recover,
                            reason,
                            resumes_when,
                        }),
                        None,
                    ),
                    Ok(Control::Pause { reason, removed }) => {
                        (Some(Halt::Pause { reason, removed }), None)
                    }
                    // A refused reattachment saves the session as a stop does.
                    Err(error) if error.is::<Refused>() => (
                        Some(Halt::Pause {
                            reason: Some(error.to_string()),
                            removed: false,
                        }),
                        None,
                    ),
                    Ok(Control::Stop { reason }) => (None, reason),
                    Ok(_) => bail!("unexpected execution control"),
                    Err(error) => return Err(error.context(
                        "daemon disconnected; command stopped, execution requires reconciliation",
                    )),
                };
                return Ok((status, recovery, stopped));
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

fn spawn(
    paths: &Paths,
    plan: &ExecutionPlan,
    command: &[OsString],
    mode: &Mode,
    auth: Option<&AgentLaunch>,
) -> Result<Child> {
    let mut process = Command::new(&command[0]);
    process
        .args(&command[1..])
        .current_dir(&plan.workspace.path);
    configure_environment(&mut process, paths, plan);
    if let Some(auth) = auth {
        auth.apply(&mut process);
    }
    export_execution(&mut process, paths, plan);
    // A demo is the user's, so the agent's output setting stays behind, and
    // its own port or device wins over a reservation exported as `PORT`.
    if let Mode::Demo { launch } = mode {
        process.env_remove(env::OUTPUT).envs(&launch.environment);
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
        Mode::Publish => (
            Stdio::inherit(),
            Stdio::from(std::io::stderr()),
            Stdio::inherit(),
        ),
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

/// Export the workspace's identity and `[env]`, dropping stale port variables
/// inherited from an enclosing execution.
fn configure_environment(process: &mut Command, paths: &Paths, plan: &ExecutionPlan) {
    env::apply_workspace_identity(process, &plan.workspace, paths);
    env::apply_configured(process, &plan.environment);
}

/// Export the execution's scope and port reservations last, so no configured
/// variable replaces or removes them.
fn export_execution(process: &mut Command, paths: &Paths, plan: &ExecutionPlan) {
    process
        .envs(env::workspace_environment(
            &plan.workspace,
            paths,
            &plan.ports,
            &plan.scope_token,
        ))
        .env(env::EXECUTION_ID, &plan.id);
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

#[cfg(test)]
mod tests;
