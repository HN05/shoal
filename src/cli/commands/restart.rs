//! Restarting agents in place, one at a time, so each runs its agent's
//! current binary in the terminal it already has.
use std::time::Duration;

use futures_util::{StreamExt, stream::FuturesUnordered};

use anyhow::{Result, ensure};
use serde_json::json;
use tokio::time::{Instant, sleep, sleep_until};

use crate::{
    agent::Agent,
    cli::{args::WorkspaceTargets, client::request, context::Context},
    daemon::workspace::capacity::STAGGER,
    execution::recovery,
    model::RunningAgent,
    protocol::{ErrorCode, ExecutionMethod, RemoteError},
};

mod plan;
mod progress;

pub(super) use plan::skip_reason;
use plan::{Restart, Selection, Skipped};
use progress::Progress;

/// How many agents restart at once, each holding one of the daemon's
/// long-lived connections until its agent runs again.
const PARALLEL_RESTARTS: usize = 16;

/// How often `--wait` checks whether a turn ended.
const TURN_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub(super) async fn run(
    ctx: &Context,
    targets: WorkspaceTargets,
    agent: Option<Agent>,
    wait: bool,
    sequential: bool,
) -> Result<i32> {
    // A process that cleared its scope token is still the workspace's.
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "restart agents outside scoped executions"
    );
    let selection = Selection::load(ctx, targets).await?;
    let plan = selection.plan(agent.as_ref().map(Agent::name));
    if plan.restarts.is_empty() && plan.skipped.is_empty() {
        let message = match &agent {
            Some(agent) => format!("No running {agent} agents"),
            None => "No running agents".into(),
        };
        ctx.emit(
            &message,
            json!({"restarted": [], "failed": [], "skipped": []}),
        )?;
        return Ok(0);
    }
    let mut progress = Progress::new(plan.restarts.len() + plan.skipped.len());
    for skipped in plan.skipped {
        progress.skip(ctx, skipped);
    }
    if sequential {
        restart_in_turn(ctx, &mut progress, plan.restarts, wait).await?;
    } else {
        restart_together(ctx, &mut progress, plan.restarts, wait).await?;
    }
    progress.finish(ctx)
}

impl Restart {
    async fn run(&self, ctx: &Context) -> Result<()> {
        let restarted = request::<()>(
            &ctx.paths,
            ExecutionMethod::RestartAgent {
                execution: self.agent.execution.clone(),
            },
        )
        .await;
        restarted.map_err(|error| {
            // A restart that waits still restores the agent itself.
            match recovery::pending(&ctx.paths, &self.workspace.id) {
                Ok(true) if waits(&error).is_none() => anyhow::anyhow!(
                    "{error:#}; restore it with shoal resume {}",
                    self.workspace.name
                ),
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
    wait: bool,
) -> Result<()> {
    let mut previous: Option<Instant> = None;
    while !restarts.is_empty() {
        if let Some(previous) = previous {
            sleep_until(previous + STAGGER).await;
        }
        let restart = if wait {
            let Some(restart) = next_idle(ctx, progress, &mut restarts).await? else {
                break;
            };
            restart
        } else {
            restarts.remove(0)
        };
        let result = restart.run(ctx).await;
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
    wait: bool,
) -> Result<()> {
    let mut running = FuturesUnordered::new();
    let mut told = false;
    loop {
        if wait {
            for gone in refresh(ctx, &mut waiting).await? {
                progress.skip(ctx, gone);
            }
        }
        while running.len() < PARALLEL_RESTARTS
            && let Some(index) = waiting.iter().position(|restart| !wait || restart.idle())
        {
            let restart = waiting.remove(index);
            running.push(async move {
                let result = restart.run(ctx).await;
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
            () = sleep(TURN_POLL_INTERVAL), if wait && !waiting.is_empty() => {}
        }
    }
}

/// Say whose turns the restarts wait for, returning whether any.
fn tell_waiting(ctx: &Context, restarts: &[Restart]) -> bool {
    let names: Vec<_> = restarts
        .iter()
        .filter(|restart| !restart.idle())
        .map(|restart| restart.workspace.name.as_str())
        .collect();
    if !names.is_empty() && !ctx.json {
        eprintln!("Waiting for the turns of {} to end", names.join(", "));
    }
    !names.is_empty()
}

/// Take the first of `restarts` whose turn ended, from states read now,
/// waiting until one has; an agent waiting for the user or without turn
/// reports works. Agents that stopped running are reported as skipped.
async fn next_idle(
    ctx: &Context,
    progress: &mut Progress,
    restarts: &mut Vec<Restart>,
) -> Result<Option<Restart>> {
    let mut told = false;
    loop {
        for gone in refresh(ctx, restarts).await? {
            progress.skip(ctx, gone);
        }
        if restarts.is_empty() {
            return Ok(None);
        }
        if let Some(index) = restarts.iter().position(Restart::idle) {
            return Ok(Some(restarts.remove(index)));
        }
        if !told {
            told = tell_waiting(ctx, restarts);
        }
        sleep(TURN_POLL_INTERVAL).await;
    }
}

/// Read each agent's turn state again, and drop the agents that no longer
/// run or whose wrapper is gone, returning them.
async fn refresh(ctx: &Context, restarts: &mut Vec<Restart>) -> Result<Vec<Skipped>> {
    let running: Vec<RunningAgent> = request(&ctx.paths, ExecutionMethod::ListAgents).await?;
    let mut ended = Vec::new();
    for mut restart in std::mem::take(restarts) {
        let current = running
            .iter()
            .find(|agent| agent.execution == restart.agent.execution);
        let reason = match current {
            None => Some("the agent no longer runs".into()),
            Some(current) => skip_reason(&restart.workspace, current),
        };
        match reason {
            Some(reason) => ended.push(Skipped {
                workspace: restart.workspace.name,
                agent: restart.agent.agent,
                reason,
            }),
            None => {
                restart.agent.state = current.and_then(|agent| agent.state);
                restarts.push(restart);
            }
        }
    }
    Ok(ended)
}
