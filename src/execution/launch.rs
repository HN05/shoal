//! A tracked execution from its daemon record to its reported completion: the
//! launch it prepares, the runs of its command, and what follows each run.
use std::{
    ffi::OsString,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use super::{
    Mode, attachment,
    completion::report_completion,
    demo,
    link::Link,
    pause::{AfterPause, Halt, pause_agent, record_stopped_command, save_ending_agent},
    recovery,
    spawn::AgentLaunch,
    supervise::{Outcome, Saving, supervise},
    takeover, wake,
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
    process::{self, identity::Identity},
    protocol::{Body, ExecutionMethod, Reattach},
    sessions::Origin,
};

/// A tracked execution the daemon recorded, with the command it runs next and
/// what that command receives.
struct TrackedLaunch {
    link: Link,
    plan: ExecutionPlan,
    command: Vec<OsString>,
    auth: Option<AgentLaunch>,
    recovery: Option<recovery::Recovery>,
    /// The saved record a restored agent consumes once it is registered.
    record: Option<PathBuf>,
}

pub(super) async fn run_tracked(
    paths: &Paths,
    workspace: String,
    command: Vec<OsString>,
    mode: Mode,
    agent: Option<String>,
) -> Result<i32> {
    let launch = prepare_tracked_launch(paths, workspace, command, &mode, agent).await?;
    launch.run_to_end(paths, &mode).await
}

/// Restore the agent of a wrapper that continues from this binary.
pub(super) async fn run_taken_over(paths: &Paths, taken: super::TakenOver) -> Result<i32> {
    let mode = match taken.log.clone() {
        Some(log) => Mode::Detached { log },
        None => Mode::Command { record: false },
    };
    let launch = prepare_taken_over_launch(paths, taken, &mode).await?;
    launch.run_to_end(paths, &mode).await
}

impl TrackedLaunch {
    /// Run the command until the execution ends, then report its end.
    async fn run_to_end(mut self, paths: &Paths, mode: &Mode) -> Result<i32> {
        let wake = self.wake_session(mode);
        let mut restarts = demo::Restarts::default();
        let result = loop {
            let started = Instant::now();
            let outcome = self.run(paths, mode, wake.clone()).await;
            if let Some(refusal) = self.link.refusal() {
                eprintln!("shoal: {refusal}; command stopped");
            }
            let ran = started.elapsed();
            // A failure after the run, such as a restored agent's proxy that
            // cannot start, still reports the execution's end; a restore record
            // it did not consume stays for `shoal resume`.
            let next = match outcome {
                Ok(outcome) => {
                    self.after_run(paths, mode, outcome, &mut restarts, ran)
                        .await
                }
                Err(error) => Err(error),
            };
            match next {
                Ok(Some(code)) => break Ok(code),
                Ok(None) => {}
                Err(error) => break Err(error),
            }
        };
        let result = roll_back_failed_land(&self.plan, result).await;
        self.finish(mode, result).await
    }

    /// The terminal session to wake the agent in when it is idle.
    fn wake_session(&self, mode: &Mode) -> Option<wake::Wake> {
        let integration = self.auth.as_ref().and_then(|auth| auth.integration);
        wake::session(integration, mode)
    }

    /// Report whether a terminal shows the agent while it runs.
    fn attachment(&self, paths: &Paths) -> Option<attachment::Monitor> {
        let shown = self.auth.as_ref()?.shown.clone()?;
        Some(attachment::start(paths, &self.plan, shown))
    }

    /// Run the command once; a setup without a configured command has
    /// nothing to run.
    async fn run(
        &mut self,
        paths: &Paths,
        mode: &Mode,
        wake: Option<wake::Wake>,
    ) -> Result<Outcome> {
        if mode.is_setup() && self.command.is_empty() {
            return Ok(Outcome::Exited(0));
        }
        // Prompts only while this child runs, which reports its state.
        let _waker = wake.map(|wake| wake::start(paths, &self.plan, wake));
        let _attachment = self.attachment(paths);
        let (plan, recovery) = (&self.plan, self.recovery.as_ref());
        supervise(
            &mut self.link,
            paths,
            plan,
            &self.command,
            mode,
            self.auth.as_ref(),
            Saving {
                restored_from: self.record.as_deref(),
                ending: |stop: &Halt| save_ending_agent(paths, plan, mode, recovery, stop),
            },
        )
        .await
    }

    /// What follows a run, `ran` long, that ended with `outcome`: the exit
    /// code to finish with, or `None` to run the command again.
    async fn after_run(
        &mut self,
        paths: &Paths,
        mode: &Mode,
        outcome: Outcome,
        restarts: &mut demo::Restarts,
        ran: Duration,
    ) -> Result<Option<i32>> {
        match outcome {
            Outcome::Exited(code) if matches!(mode, Mode::Demo { .. }) => {
                self.restart_demo(code, restarts.after(ran)).await
            }
            Outcome::Exited(code) => Ok(Some(code)),
            Outcome::Stopped { code, reason } => {
                self.report_stopped(&reason);
                Ok(Some(code))
            }
            // Only an agent's session is saved when its own session ends.
            Outcome::Paused {
                code,
                stop: Halt::Ended { .. },
            } if self.recovery.is_none() => Ok(Some(code)),
            Outcome::Paused { code, stop } => {
                let restored = self.resolve_pause(paths, mode, &stop).await?;
                Ok((!restored).then_some(code))
            }
        }
    }

    /// Tell the user the daemon stopped the agent or command for `reason`.
    fn report_stopped(&self, reason: &str) {
        let stopped = if self.recovery.is_some() {
            "agent"
        } else {
            "command"
        };
        eprintln!("shoal: {stopped} stopped: {reason}");
    }

    /// Wait `delay` before the demo's next run: `None` to run it again, or
    /// the exited run's `code` once the demo stopped.
    async fn restart_demo(&mut self, code: i32, delay: Duration) -> Result<Option<i32>> {
        eprintln!(
            "shoal: demo exited with status {code}; restarting in {}s",
            delay.as_secs()
        );
        let Some(reason) = demo::next_run(&mut self.link, delay).await? else {
            return Ok(None);
        };
        eprintln!("shoal: demo stopped: {reason}");
        Ok(Some(code))
    }

    /// Pause the agent, or keep a stopped command for `shoal resume`; true
    /// when the daemon restored the agent to run again.
    async fn resolve_pause(&mut self, paths: &Paths, mode: &Mode, stop: &Halt) -> Result<bool> {
        let Some(recovery) = &self.recovery else {
            if matches!(mode, Mode::Command { record: true }) {
                record_stopped_command(paths, &self.plan, &self.command, stop);
            }
            return Ok(false);
        };
        match pause_agent(&mut self.link, paths, &self.plan, mode, recovery, stop).await? {
            AfterPause::Finish => Ok(false),
            AfterPause::Restart {
                command,
                proxy,
                ports,
                record,
            } => {
                // The restore command may name another proxy, or none; what
                // the old one reported goes with it.
                if let Some(auth) = self.auth.as_mut()
                    && auth.restart_proxy(proxy, &self.plan.environment).await?
                {
                    let (workspace, execution) = (&self.plan.workspace.id, &self.plan.id);
                    crate::proxy::forget_reports(paths, workspace, execution).await;
                    auth.report_proxy(paths, workspace, execution);
                }
                self.plan.ports = ports;
                self.command = command;
                self.record = record;
                Ok(true)
            }
        }
    }

    /// Report the result's exit code unless the daemon refused the
    /// execution; setup fails when the report does.
    async fn finish(mut self, mode: &Mode, result: Result<i32>) -> Result<i32> {
        if self.link.refusal().is_some() {
            return result;
        }
        let code = result.as_ref().copied().unwrap_or(1);
        // Report only after child/process-group cleanup. A lost connection never
        // grants the daemon permission to assume processes stopped.
        let report = report_completion(&mut self.link, code, mode).await;
        if mode.is_setup() {
            report?;
        } else if let Err(error) = report {
            eprintln!("warning: unable to report execution completion: {error:#}");
        }
        result
    }
}

/// Undo the land of a command that failed; a failed rollback replaces the
/// result.
async fn roll_back_failed_land(plan: &ExecutionPlan, result: Result<i32>) -> Result<i32> {
    if !matches!(result, Ok(0))
        && let Some(land) = &plan.land
        && let Err(error) = crate::git::repo::rollback_land(land).await
    {
        return Err(error);
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
    // An agent's proxy reports through the installed binary after an
    // upgrade, so the binary is read now, before the upgrade can replace it.
    if agent.is_some() {
        crate::cli::installed::remember_started();
    }
    let place = terminal_place(mode).await;
    let mut auth =
        prepare_agent_launch(paths, &workspace, mode, agent.as_deref(), place.as_ref()).await?;
    let recovery = if let Some(agent) = &agent {
        Some(recovery::Recovery::for_launch(paths, &workspace, agent).await)
    } else {
        None
    };
    let recover = recovery.as_ref().is_some_and(|recovery| recovery.automatic);
    let (mut link, plan) = open_execution(paths, workspace, mode, agent, recover, place).await?;
    // The daemon has registered the execution, so a launch that fails from
    // here reports its end, which lets the daemon forget it.
    let command = match ready_command(paths, &plan, command, mode, auth.as_mut()).await {
        Ok(command) => command,
        Err(error) => return Err(fail_launch(&mut link, mode, error).await),
    };
    Ok(TrackedLaunch {
        link,
        plan,
        command,
        auth,
        recovery,
        record: None,
    })
}

/// The launch of a wrapper that continues from this binary: it registers
/// again over the connection it was handed, then restores its agent from the
/// record.
async fn prepare_taken_over_launch(
    paths: &Paths,
    taken: super::TakenOver,
    mode: &Mode,
) -> Result<TrackedLaunch> {
    crate::cli::installed::remember_started();
    let super::TakenOver {
        workspace,
        agent,
        connection,
        record,
        ..
    } = taken;
    let mut link = takeover::connection(connection)?;
    let place = terminal_place(mode).await;
    let registered = async {
        let auth =
            prepare_agent_launch(paths, &workspace, mode, Some(&agent), place.as_ref()).await?;
        let recovery = recovery::Recovery::for_launch(paths, &workspace, &agent).await;
        let plan = takeover::register_again(&mut link, place).await?;
        anyhow::Ok((auth, recovery, plan, this_wrapper()?))
    }
    .await;
    let (mut auth, recovery, plan, wrapper) = match registered {
        Ok(registered) => registered,
        Err(error) => return Err(fail_launch(&mut link, mode, error).await),
    };
    allow_reattach(
        &mut link,
        paths,
        &plan,
        wrapper,
        Some(agent.clone()),
        recovery.automatic,
    );
    let (command, proxy) = recovery
        .restore_command(paths, &plan.workspace, record.as_deref())
        .await;
    // The restore command was resolved to go through `proxy`, from the same
    // settings.
    if let Some(auth) = auth.as_mut() {
        auth.routed = proxy;
    }
    let command = match ready_command(paths, &plan, command, mode, auth.as_mut()).await {
        Ok(command) => command,
        Err(error) => return Err(fail_launch(&mut link, mode, error).await),
    };
    Ok(TrackedLaunch {
        link,
        plan,
        command,
        auth,
        recovery: Some(recovery),
        record,
    })
}

/// Report the end of a registered execution whose launch failed with `error`,
/// which lets the daemon forget it, and return the error.
async fn fail_launch(link: &mut Link, mode: &Mode, error: anyhow::Error) -> anyhow::Error {
    if let Err(report) = report_completion(link, 1, mode).await {
        eprintln!("warning: unable to report execution completion: {report:#}");
    }
    error
}

/// Where the wrapper runs in a terminal host. A detached command or a demo
/// never runs in the caller's terminal.
async fn terminal_place(mode: &Mode) -> Option<Origin> {
    match mode {
        Mode::Detached { .. } | Mode::Demo { .. } => None,
        _ => crate::cli::terminal::current_place().await,
    }
}

/// Start the agent's proxy for the registered `plan`, and the command it
/// runs.
async fn ready_command(
    paths: &Paths,
    plan: &ExecutionPlan,
    command: Vec<OsString>,
    mode: &Mode,
    auth: Option<&mut AgentLaunch>,
) -> Result<Vec<OsString>> {
    if let Some(auth) = auth {
        auth.start_proxy(&plan.environment).await?;
        auth.report_proxy(paths, &plan.workspace.id, &plan.id);
    }
    execution_command(paths, plan, command, mode)
}

async fn open_execution(
    paths: &Paths,
    workspace: String,
    mode: &Mode,
    agent: Option<String>,
    recover: bool,
    place: Option<Origin>,
) -> Result<(Link, ExecutionPlan)> {
    let wrapper = this_wrapper()?;
    let method = ExecutionMethod::Execute {
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
        origin: place.map(Box::new),
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
        allow_reattach(&mut link, paths, &plan, wrapper, agent, recover);
    }
    Ok((link, plan))
}

/// This process, as the daemon records an execution's wrapper.
fn this_wrapper() -> Result<Identity> {
    process::identity::capture(std::process::id())?.context("cannot identify execution wrapper")
}

/// Let the execution of `plan`, which `wrapper` runs, reattach to the next
/// daemon once its command runs.
fn allow_reattach(
    link: &mut Link,
    paths: &Paths,
    plan: &ExecutionPlan,
    wrapper: Identity,
    agent: Option<String>,
    recover: bool,
) {
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

async fn prepare_agent_launch(
    paths: &Paths,
    workspace: &str,
    mode: &Mode,
    agent: Option<&str>,
    place: Option<&Origin>,
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
    // An agent Shoal cannot point at its proxy does not start.
    let routed = match agent {
        Some(agent) => crate::cli::agents::launch_proxy(&settings, agent)?.cloned(),
        None => None,
    };
    let fetched_key = super::spawn::fetch_command_key(routed.as_ref()).await?;
    Ok(Some(AgentLaunch {
        account: settings.agent_auth.prepare(paths, &config.git)?,
        secrets,
        output: has_agent.then(env::agent_output).flatten(),
        integration: agent.map(|agent| settings.integration(agent)),
        environment: agent
            .map(|agent| settings.launch(agent).environment.clone())
            .unwrap_or_default(),
        home: paths.home.clone(),
        routed,
        fetched_key,
        proxy: None,
        shown: has_agent
            .then(|| attachment::Shown::of(&settings, place))
            .flatten(),
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
