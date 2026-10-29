//! Restarting agents in place, one at a time, so each runs its agent's
//! current binary in the terminal it already has.
use std::time::Duration;

use futures_util::{StreamExt, stream::FuturesUnordered};

use anyhow::{Result, ensure};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep, sleep_until};

use crate::{
    agent::Agent,
    cli::{
        args::WorkspaceTargets,
        client::request,
        context::Context,
        output::{Palette, Style},
        ui,
    },
    daemon::workspace::capacity::STAGGER,
    execution::recovery,
    model::RunningAgent,
    protocol::{ErrorCode, ExecutionMethod, RemoteError},
};

mod plan;

pub(super) use plan::skip_reason;
use plan::{Restart, Selection, Skipped};

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
    let mut report = ui::Report::new("restarted", "restart");
    let mut skipped = report_skipped(ctx, &plan.skipped);
    let gone = if sequential {
        restart_in_turn(ctx, &mut report, plan.restarts, wait).await?
    } else {
        restart_together(ctx, &mut report, plan.restarts, wait).await?
    };
    skipped.extend(gone);
    report.field("skipped", skipped.into());
    report.finish(ctx, "")
}

/// Print each skipped agent, returning their JSON entries.
fn report_skipped(ctx: &Context, skipped: &[Skipped]) -> Vec<Value> {
    let palette = Palette::stdout(ctx.json);
    skipped
        .iter()
        .map(|skipped| {
            if !ctx.json {
                let message = format!("Skipped {}: {}", skipped.workspace, skipped.reason);
                outln!("{}", palette.paint(Style::Warning, &message));
            }
            json!({
                "workspace": skipped.workspace,
                "agent": skipped.agent,
                "reason": skipped.reason,
            })
        })
        .collect()
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
            let waits = error
                .downcast_ref::<RemoteError>()
                .is_some_and(|remote| remote.code == ErrorCode::RestartWaits);
            match recovery::pending(&ctx.paths, &self.workspace.id) {
                Ok(true) if !waits => anyhow::anyhow!(
                    "{error:#}; restore it with shoal resume {}",
                    self.workspace.name
                ),
                _ => error,
            }
        })
    }
}

/// Restart `restarts` one at a time, idle ones first, each at least the
/// admission stagger after the previous start, and with `wait` only once its
/// turn ended,
/// returning the JSON entries of agents that stopped running meanwhile.
async fn restart_in_turn(
    ctx: &Context,
    report: &mut ui::Report,
    mut restarts: Vec<Restart>,
    wait: bool,
) -> Result<Vec<Value>> {
    let total = restarts.len();
    let mut position = 0;
    let mut previous: Option<Instant> = None;
    let mut gone = Vec::new();
    while !restarts.is_empty() {
        if let Some(previous) = previous {
            sleep_until(previous + STAGGER).await;
        }
        let restart = if wait {
            let Some(restart) = next_idle(ctx, &mut restarts, &mut gone).await? else {
                break;
            };
            restart
        } else {
            restarts.remove(0)
        };
        position += 1;
        let result = restart.run(ctx).await;
        if result.is_ok() {
            previous = Some(Instant::now());
        }
        report_outcome(ctx, report, &restart, result, (position, total));
    }
    Ok(gone)
}

/// Restart `restarts` together, up to [`PARALLEL_RESTARTS`] at a time, and
/// with `wait` each as soon as its turn ends, returning the JSON entries of
/// agents that stopped running meanwhile.
async fn restart_together(
    ctx: &Context,
    report: &mut ui::Report,
    mut waiting: Vec<Restart>,
    wait: bool,
) -> Result<Vec<Value>> {
    let total = waiting.len();
    let mut finished = 0;
    let mut gone = Vec::new();
    let mut running = FuturesUnordered::new();
    let mut told = false;
    loop {
        if wait {
            gone.extend(report_skipped(ctx, &refresh(ctx, &mut waiting).await?));
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
            return Ok(gone);
        }
        if wait && !told {
            told = tell_waiting(ctx, &waiting);
        }
        tokio::select! {
            Some((restart, result)) = running.next() => {
                finished += 1;
                report_outcome(ctx, report, &restart, result, (finished, total));
            }
            () = sleep(TURN_POLL_INTERVAL), if wait && !waiting.is_empty() => {}
        }
    }
}

/// Report how `restart` ended, as the `position`th of `total`.
fn report_outcome(
    ctx: &Context,
    report: &mut ui::Report,
    restart: &Restart,
    result: Result<()>,
    (position, total): (usize, usize),
) {
    let name = &restart.workspace.name;
    match result {
        Ok(()) => report.done(
            ctx,
            &format!("Restarted {name} ({position}/{total})"),
            json!({"workspace": name, "agent": restart.agent.agent}),
        ),
        Err(error) => report.failed(ctx, name, &error),
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
/// reports works. Agents that stopped running are reported into `gone`.
async fn next_idle(
    ctx: &Context,
    restarts: &mut Vec<Restart>,
    gone: &mut Vec<Value>,
) -> Result<Option<Restart>> {
    let mut told = false;
    loop {
        gone.extend(report_skipped(ctx, &refresh(ctx, restarts).await?));
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
