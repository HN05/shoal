//! A tracked execution from its daemon record to its reported completion: the
//! launch it prepares, the runs of its command, and what follows each run.
use std::{ffi::OsString, time::Instant};

use anyhow::{Context, Result};

use super::{
    Mode,
    completion::report_completion,
    demo,
    link::Link,
    pause::{AfterPause, Halt, pause_agent, record_stopped_command},
    recovery,
    spawn::AgentLaunch,
    supervise::{Outcome, supervise},
    wake,
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

struct TrackedLaunch {
    link: Link,
    plan: ExecutionPlan,
    command: Vec<OsString>,
    auth: Option<AgentLaunch>,
    recovery: Option<recovery::Recovery>,
}

pub(super) async fn run_tracked(
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
