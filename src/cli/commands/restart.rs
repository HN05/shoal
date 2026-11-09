//! Restarting agents, each in a new wrapper or in place, together or one at
//! a time, so each runs its agent's current binary. A chat's agent restarts
//! in a new place, as `shoal resume` restores a chat.
use std::time::Duration;

use futures_util::{StreamExt, stream::FuturesUnordered};

use anyhow::{Result, ensure};
use tokio::time::{Instant, sleep, sleep_until};

use crate::client::output::agents::Restarted;
use crate::client::space_names::SpaceSelector;
use crate::{
    agent::Agent,
    cli::{args::SpaceTargets, context::Context},
    client::{agents, executions::STAGGER},
    config::permissions::PermissionChange,
    execution::recovery,
    model::{PresentSpace, RunningAgent, Workspace},
    protocol::{ErrorCode, RemoteError},
    sessions::Origin,
};

mod plan;
mod progress;

pub(super) use plan::skip_reason;
use plan::{Restart, Selection, Skipped, running_agents, space_skip_reason, stopped_reason};
use progress::Progress;

/// How many agents restart at once, each holding one of the daemon's
/// long-lived connections until its agent runs again.
const PARALLEL_RESTARTS: usize = 16;

/// How often `--wait` checks whether a turn ended.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How a run restarts the agents it selects.
#[derive(Clone)]
pub(super) struct Options {
    /// Restart each agent only once its turn ends.
    pub wait: bool,
    /// Restart one agent at a time.
    pub sequential: bool,
    /// Restart each agent in its wrapper rather than a new one.
    pub in_place: bool,
    /// What changes in the permissions each restarted agent was given.
    pub change: Option<PermissionChange>,
}

pub(super) async fn run(
    ctx: &Context,
    targets: SpaceTargets,
    agent: Option<Agent>,
    options: Options,
) -> Result<i32> {
    let scoped = crate::env::inherits_scope(&ctx.paths.state);
    if scoped {
        ensure_named(&targets)?;
    }
    let selection = Selection::load(ctx, targets).await?;
    if scoped {
        selection.ensure_below()?;
    }
    let plan = selection.plan(agent.as_ref().map(Agent::name), options.in_place);
    if plan.restarts.is_empty() && plan.skipped.is_empty() {
        let message = match &agent {
            Some(agent) => format!("No running {agent} agents"),
            None => "No running agents".into(),
        };
        ctx.emit(
            &message,
            Restarted {
                restarted: Vec::new(),
                failed: Vec::new(),
                skipped: Vec::new(),
                interrupted: false,
                pending: Vec::new(),
            },
        )?;
        return Ok(0);
    }
    if let Some(change) = &options.change {
        ensure_change_fits(ctx, &plan.restarts, change).await?;
    }
    let mut progress = Progress::new(&plan.restarts, plan.skipped.len());
    for skipped in plan.skipped {
        progress.skip(ctx, skipped);
    }
    let interrupted = tokio::select! {
        restarted = restart_all(ctx, &mut progress, plan.restarts, &plan.run, &options) => {
            restarted?;
            false
        }
        signal = tokio::signal::ctrl_c() => {
            signal?;
            true
        }
    };
    if interrupted {
        // The daemon goes on with the restarts it was asked for.
        let agents = running_agents(ctx).await?;
        return progress.interrupted(ctx, &agents, agent.as_ref().map(Agent::name));
    }
    progress.finish(ctx)
}

/// Refuse a restart in a scoped process, a workspace's or a chat's agent,
/// that does not name the spaces it restarts. The daemon refuses each space
/// not below the caller's, which needs the scope token: a process that
/// cleared it stays refused.
fn ensure_named(targets: &SpaceTargets) -> Result<()> {
    ensure!(
        crate::env::is_scoped() && !targets.all && !targets.space.is_empty(),
        "scoped processes restart only the spaces below their own, by name: shoal restart <space>"
    );
    Ok(())
}

/// Refuse `change` before any agent restarts where this scoped process
/// does not hold what it gives an agent `restarts` restarts, or where one
/// joins a restart in progress, which keeps the permissions it started with.
async fn ensure_change_fits(
    ctx: &Context,
    restarts: &[Restart],
    change: &PermissionChange,
) -> Result<()> {
    for restart in restarts {
        ensure!(
            restart.joins.is_none(),
            "{} restarts already; restart it with --permission once it runs",
            restart.space.name()
        );
        let repository = restart.space.repository_id().map(Into::into);
        super::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    Ok(())
}

async fn restart_all(
    ctx: &Context,
    progress: &mut Progress,
    restarts: Vec<Restart>,
    run: &str,
    options: &Options,
) -> Result<()> {
    if options.sequential {
        restart_in_turn(ctx, progress, restarts, run, options).await
    } else {
        restart_together(ctx, progress, restarts, run, options).await
    }
}

impl Restart {
    /// Restart the agent as `options` say, or join its restart, as part of
    /// `run`, returning the place it runs in now.
    async fn run(&self, ctx: &Context, run: &str, options: &Options) -> Result<Option<Origin>> {
        let run = Some(run.to_owned());
        match &self.space {
            // The daemon names how to restore a chat whose restart failed.
            PresentSpace::Chat(chat) => {
                agents::restart_chat(
                    &ctx.paths,
                    SpaceSelector::id(&chat.id),
                    self.joins.clone(),
                    run,
                    options.change.clone(),
                )
                .await
            }
            PresentSpace::Workspace(workspace) => {
                self.restart_agent(ctx, workspace, run, options).await
            }
        }
    }

    /// [`Self::run`] for the agent of `workspace`, naming how to restore it
    /// when its restart failed.
    async fn restart_agent(
        &self,
        ctx: &Context,
        workspace: &Workspace,
        run: Option<String>,
        options: &Options,
    ) -> Result<Option<Origin>> {
        let execution = self.agent.execution.clone();
        let joins = self.joins.clone();
        let (in_place, change) = (options.in_place, options.change.clone());
        let restarted = agents::restart(&ctx.paths, execution, joins, run, in_place, change).await;
        restarted.map_err(|error| {
            // A restart that waits still restores the agent itself, as a
            // wrapper waiting for its restore does.
            match recovery::resumable(&ctx.paths, &workspace.id) {
                Ok(true) if waits(&error).is_none() => {
                    anyhow::anyhow!("{error:#}; restore it with shoal resume {}", workspace.name)
                }
                _ => error,
            }
        })
    }
}

/// What the restart waits for, when `error` answers a restart that goes on
/// once the stopped agent's remaining processes exit.
fn waits(error: &anyhow::Error) -> Option<&str> {
    error
        .downcast_ref::<RemoteError>()
        .filter(|remote| remote.code == ErrorCode::RestartWaits)
        .map(|remote| remote.message.as_str())
}

/// Restart `restarts` one at a time, idle ones first, each at least the
/// admission stagger after the previous start, and with `wait` only once its
/// turn ended.
async fn restart_in_turn(
    ctx: &Context,
    progress: &mut Progress,
    mut restarts: Vec<Restart>,
    run: &str,
    options: &Options,
) -> Result<()> {
    let mut previous: Option<Instant> = None;
    while !restarts.is_empty() {
        if let Some(previous) = previous {
            sleep_until(previous + STAGGER).await;
        }
        let restart = if options.wait {
            let Some(restart) = next_idle(ctx, progress, &mut restarts, run).await? else {
                break;
            };
            restart
        } else {
            restarts.remove(0)
        };
        let result = restart.run(ctx, run, options).await;
        if result.is_ok() {
            previous = Some(Instant::now());
        }
        progress.ended(ctx, &restart, result);
    }
    Ok(())
}

/// Restart `restarts` together, up to [`PARALLEL_RESTARTS`] at a time, and
/// with `wait` each as soon as its turn ends.
async fn restart_together(
    ctx: &Context,
    progress: &mut Progress,
    mut waiting: Vec<Restart>,
    run: &str,
    options: &Options,
) -> Result<()> {
    let wait = options.wait;
    let mut running = FuturesUnordered::new();
    let mut told = false;
    loop {
        if wait {
            for gone in refresh(&mut waiting, &running_agents(ctx).await?, run) {
                progress.skip(ctx, gone);
            }
        }
        while running.len() < PARALLEL_RESTARTS
            && let Some(index) = waiting.iter().position(|restart| !wait || restart.ready())
        {
            let restart = waiting.remove(index);
            running.push(async move {
                let result = restart.run(ctx, run, options).await;
                (restart, result)
            });
        }
        if waiting.is_empty() && running.is_empty() {
            return Ok(());
        }
        if wait && !told {
            told = tell_waiting(ctx, &waiting);
        }
        tokio::select! {
            Some((restart, result)) = running.next() => progress.ended(ctx, &restart, result),
            () = sleep(POLL_INTERVAL), if wait && !waiting.is_empty() => {}
        }
    }
}

/// Say whose turns the restarts wait for, returning whether any.
fn tell_waiting(ctx: &Context, restarts: &[Restart]) -> bool {
    let names: Vec<_> = restarts
        .iter()
        .filter(|restart| !restart.ready())
        .map(|restart| restart.space.name())
        .collect();
    if !names.is_empty() && !ctx.json {
        eprintln!("Waiting for the turns of {} to end", names.join(", "));
    }
    !names.is_empty()
}

/// Take the first of `restarts` whose turn ended, from states read now,
/// waiting until one has; an agent waiting for the user or without turn
/// reports works. Agents that stopped running, or that another command of
/// `run` restarted, are reported as skipped.
async fn next_idle(
    ctx: &Context,
    progress: &mut Progress,
    restarts: &mut Vec<Restart>,
    run: &str,
) -> Result<Option<Restart>> {
    let mut told = false;
    loop {
        for gone in refresh(restarts, &running_agents(ctx).await?, run) {
            progress.skip(ctx, gone);
        }
        if restarts.is_empty() {
            return Ok(None);
        }
        if let Some(index) = restarts.iter().position(Restart::ready) {
            return Ok(Some(restarts.remove(index)));
        }
        if !told {
            told = tell_waiting(ctx, restarts);
        }
        sleep(POLL_INTERVAL).await;
    }
}

/// Take each agent's turn state and restart from `running`, where a
/// restart may have started it in a new wrapper, and drop the agents that no
/// longer run, whose wrapper is gone, that protection stopped, or that
/// another command of `run` restarted meanwhile, returning them.
fn refresh(restarts: &mut Vec<Restart>, running: &[RunningAgent], run: &str) -> Vec<Skipped> {
    let mut ended = Vec::new();
    for mut restart in std::mem::take(restarts) {
        let Some(current) = running
            .iter()
            .find(|agent| agent.ran_as(&restart.agent.execution))
        else {
            ended.push(restart.skipped("the agent no longer runs"));
            continue;
        };
        let restarted = restart.joins.is_none() && current.restarted_by.iter().any(|by| by == run);
        let reason = stopped_reason(restart.space.name(), current)
            .or_else(|| space_skip_reason(&restart.space, current));
        match reason {
            Some(reason) => ended.push(restart.skipped(reason)),
            None if restarted => {
                ended.push(restart.skipped("another command of this run restarted it already"));
            }
            None => {
                restart.agent = current.clone();
                restarts.push(restart);
            }
        }
    }
    ended
}

#[cfg(test)]
mod tests;
