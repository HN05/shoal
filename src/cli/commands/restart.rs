//! Restarting agents in place, one at a time, so each runs its agent's
//! current binary in the terminal it already has.
use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep, sleep_until};

use crate::{
    agent::Agent,
    cli::{
        args::WorkspaceTargets,
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui,
    },
    daemon::workspace::capacity::STAGGER,
    execution::recovery,
    model::{RunningAgent, Workspace},
    protocol::ExecutionMethod,
    state::AgentState,
};

/// How often `--wait` checks whether a turn ended.
const TURN_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub(super) async fn run(
    ctx: &Context,
    targets: WorkspaceTargets,
    agent: Option<Agent>,
    wait: bool,
) -> Result<i32> {
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
    report.field("skipped", report_skipped(ctx, &plan.skipped));
    restart_in_turn(ctx, &mut report, plan.restarts, wait).await?;
    report.finish(ctx, "")
}

/// The selected workspaces and the agents that run there.
struct Selection {
    workspaces: Vec<Workspace>,
    agents: Vec<RunningAgent>,
    /// The workspaces were named, so one without an agent is reported.
    named: bool,
}

/// What a run restarts, in order, and what it skips.
struct Plan {
    restarts: Vec<Restart>,
    skipped: Vec<Skipped>,
}

/// One agent to restart.
struct Restart {
    workspace: Workspace,
    agent: RunningAgent,
}

/// A workspace whose agent cannot restart in place.
struct Skipped {
    workspace: String,
    agent: Option<String>,
    reason: String,
}

impl Selection {
    /// `--all` selects every workspace, names select theirs, and neither the
    /// picker.
    async fn load(ctx: &Context, targets: WorkspaceTargets) -> Result<Self> {
        let named = !targets.all && !targets.workspace.is_empty();
        let mut workspaces = client::workspaces(&ctx.paths).await?;
        if !targets.all {
            let selected = ui::select_workspaces(ctx, targets.workspace).await?;
            workspaces = in_order(ctx, workspaces, selected).await?;
        }
        let agents = request(&ctx.paths, ExecutionMethod::ListAgents).await?;
        Ok(Self {
            workspaces,
            agents,
            named,
        })
    }

    /// The agents named `agent` to restart, idle ones first, and those that
    /// cannot restart in place.
    fn plan(self, agent: Option<&str>) -> Plan {
        let mut plan = Plan {
            restarts: Vec::new(),
            skipped: Vec::new(),
        };
        for workspace in self.workspaces {
            // An execution whose wrapper is gone may run any agent.
            let agents: Vec<_> = self
                .agents
                .iter()
                .filter(|running| running.workspace_id == workspace.id)
                .filter(|running| {
                    agent.is_none() || running.agent.is_none() || running.agent.as_deref() == agent
                })
                .cloned()
                .collect();
            if agents.is_empty() && self.named {
                plan.skipped.push(Skipped {
                    workspace: workspace.name.clone(),
                    agent: agent.map(str::to_owned),
                    reason: match agent {
                        Some(agent) => format!("no {agent} agent runs there"),
                        None => "no agent runs there".into(),
                    },
                });
            }
            for running in agents {
                match skip_reason(&workspace, &running) {
                    Some(reason) => plan.skipped.push(Skipped {
                        workspace: workspace.name.clone(),
                        agent: running.agent,
                        reason,
                    }),
                    None => plan.restarts.push(Restart {
                        workspace: workspace.clone(),
                        agent: running,
                    }),
                }
            }
        }
        plan.restarts.sort_by_key(|restart| !restart.idle());
        plan
    }
}

/// The workspaces `selected` names, in its order. A selector no listed
/// workspace has is inspected, which names why the daemon does not know it.
async fn in_order(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    selected: Vec<String>,
) -> Result<Vec<Workspace>> {
    let mut by_id: HashMap<_, _> = workspaces
        .into_iter()
        .map(|workspace| (workspace.id.clone(), workspace))
        .collect();
    let mut ordered = Vec::new();
    for selector in selected {
        ordered.push(match by_id.remove(&selector) {
            Some(workspace) => workspace,
            None => client::inspect(&ctx.paths, selector).await?.workspace,
        });
    }
    Ok(ordered)
}

/// Why `running` cannot restart in place, if it cannot.
fn skip_reason(workspace: &Workspace, running: &RunningAgent) -> Option<String> {
    let manual = format!("use shoal stop {0} and shoal resume {0}", workspace.name);
    match &running.agent {
        None => Some(format!(
            "execution {} is disconnected; {manual}",
            running.execution
        )),
        Some(agent) if !running.restorable => Some(format!(
            "{agent} has no command to resume its session; {manual}"
        )),
        Some(_) => None,
    }
}

/// Print each skipped agent, returning their JSON entries.
fn report_skipped(ctx: &Context, skipped: &[Skipped]) -> Value {
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
    fn idle(&self) -> bool {
        self.workspace
            .agent_state
            .as_ref()
            .is_some_and(|status| status.state == AgentState::Idle)
    }

    async fn run(&self, ctx: &Context) -> Result<()> {
        let restarted = request::<()>(
            &ctx.paths,
            ExecutionMethod::RestartAgent {
                execution: self.agent.execution.clone(),
            },
        )
        .await;
        restarted.map_err(
            |error| match recovery::pending(&ctx.paths, &self.workspace.id) {
                Ok(true) => anyhow::anyhow!(
                    "{error:#}; restore it with shoal resume {}",
                    self.workspace.name
                ),
                _ => error,
            },
        )
    }
}

/// Restart `restarts` one at a time, each at least the admission stagger
/// after the previous start, and with `wait` only once its turn ended.
async fn restart_in_turn(
    ctx: &Context,
    report: &mut ui::Report,
    mut restarts: Vec<Restart>,
    wait: bool,
) -> Result<()> {
    let total = restarts.len();
    let mut previous: Option<Instant> = None;
    for position in 1..=total {
        let index = if wait {
            next_idle(ctx, &mut restarts).await?
        } else {
            0
        };
        let restart = restarts.remove(index);
        if let Some(previous) = previous {
            sleep_until(previous + STAGGER).await;
        }
        let name = &restart.workspace.name;
        match restart.run(ctx).await {
            Ok(()) => {
                previous = Some(Instant::now());
                report.done(
                    ctx,
                    &format!("Restarted {name} ({position}/{total})"),
                    json!({"workspace": name, "agent": restart.agent.agent}),
                );
            }
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    Ok(())
}

/// The position of the first of `restarts` whose turn ended, waiting until
/// one has; an agent waiting for the user or without turn reports works.
async fn next_idle(ctx: &Context, restarts: &mut [Restart]) -> Result<usize> {
    let mut told = false;
    loop {
        if let Some(index) = restarts.iter().position(Restart::idle) {
            return Ok(index);
        }
        if !told && !ctx.json {
            let names: Vec<_> = restarts
                .iter()
                .map(|restart| restart.workspace.name.as_str())
                .collect();
            eprintln!("Waiting for the turns of {} to end", names.join(", "));
            told = true;
        }
        sleep(TURN_POLL_INTERVAL).await;
        refresh_turns(ctx, restarts).await?;
    }
}

/// Read each workspace's agent state again.
async fn refresh_turns(ctx: &Context, restarts: &mut [Restart]) -> Result<()> {
    let states: HashMap<_, _> = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .map(|workspace| (workspace.id, workspace.agent_state))
        .collect();
    for restart in restarts {
        if let Some(state) = states.get(&restart.workspace.id) {
            restart.workspace.agent_state = *state;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
