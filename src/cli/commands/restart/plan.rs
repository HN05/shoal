//! Which agents a run restarts, in what order, and which it skips.
use std::collections::HashMap;

use anyhow::Result;

use crate::{
    cli::{args::WorkspaceTargets, client, context::Context, ui},
    model::{RunningAgent, Workspace},
    protocol::ExecutionMethod,
    state::AgentState,
};

/// The selected workspaces and the agents that run there.
pub(super) struct Selection {
    workspaces: Vec<Workspace>,
    agents: Vec<RunningAgent>,
    /// The workspaces were named, so one without an agent is reported.
    named: bool,
}

/// What a run restarts, in order, and what it skips.
pub(super) struct Plan {
    pub(super) restarts: Vec<Restart>,
    pub(super) skipped: Vec<Skipped>,
}

/// One agent to restart.
pub(super) struct Restart {
    pub(super) workspace: Workspace,
    pub(super) agent: RunningAgent,
}

/// A workspace whose agent cannot restart in place.
pub(super) struct Skipped {
    pub(super) workspace: String,
    pub(super) agent: Option<String>,
    pub(super) reason: String,
}

impl Restart {
    /// Whether the agent itself reported that its turn ended; another
    /// agent's report in the same workspace does not count.
    pub(super) fn idle(&self) -> bool {
        self.agent.state == Some(AgentState::Idle)
    }
}

impl Selection {
    /// `--all` selects every workspace, names select theirs, and neither the
    /// picker.
    pub(super) async fn load(ctx: &Context, targets: WorkspaceTargets) -> Result<Self> {
        let named = !targets.all && !targets.workspace.is_empty();
        let mut workspaces = client::workspaces(&ctx.paths).await?;
        if !targets.all {
            let selected = ui::select_workspaces(ctx, targets.workspace).await?;
            workspaces = in_order(ctx, workspaces, selected).await?;
        }
        let agents = client::request(&ctx.paths, ExecutionMethod::ListAgents).await?;
        Ok(Self {
            workspaces,
            agents,
            named,
        })
    }

    /// The agents named `agent` to restart, idle ones first, and those that
    /// cannot restart in place.
    pub(super) fn plan(self, agent: Option<&str>) -> Plan {
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

/// Why `running` cannot restart in place, if it cannot; `shoal queue move`
/// refuses such an agent too.
pub(in crate::cli::commands) fn skip_reason(
    workspace: &Workspace,
    running: &RunningAgent,
) -> Option<String> {
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

#[cfg(test)]
mod tests;
