//! Tracked execution wrapper. The CLI process launches the command inside the
//! workspace, hands it the terminal, forwards daemon stop requests, and reports
//! completion. The daemon never touches terminal I/O. A detached launch runs
//! the same wrapper in a background `shoal` process with a log file instead
//! of the terminal, so the invoking CLI returns once the launch is recorded.
mod completion;
mod demo;
mod detached;
mod link;
mod mode;
mod pause;
pub mod recovery;
mod spawn;
mod supervise;
mod terminal;
mod wake;

use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsString,
    io::{IsTerminal, Write},
    path::PathBuf,
    time::Instant,
};

use crate::{
    cli::{
        client,
        internal::{Worker, internal_command},
    },
    daemon::approvals::Undecided,
    env,
    model::ExecutionPlan,
    paths::Paths,
    process,
    protocol::{Body, Method, Reattach},
};
pub use completion::SetupVerificationFailure;
use completion::report_completion;
#[cfg(feature = "happy")]
pub use detached::launch_detached;
use link::Link;
use mode::Mode;
use pause::{AfterPause, Halt, pause_agent, record_stopped_command};
use spawn::AgentLaunch;
pub use supervise::exit_code;
use supervise::{Outcome, supervise};
pub(crate) use terminal::{Terminal, reset_input_modes, run_in_terminal};

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
