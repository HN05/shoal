//! Interrupting an agent's turn without stopping the agent: Shoal types the
//! agent's own interrupt key into the place it runs in, through that place's
//! terminal host, and the daemon records that the interrupt ended the turn.
use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::json;

use crate::{
    cli::{
        agents,
        client::{self, request},
        context::Context,
        output::Style,
        terminal, ui,
    },
    config::Effective,
    model::{AgentStatus, RunningAgent, Workspace},
    protocol::{AgentMethod, ConfigTarget},
    sessions::Origin,
    state::AgentState,
};

/// An agent whose turn Shoal interrupted.
#[derive(Debug, Serialize)]
struct Turn {
    workspace: String,
    agent: String,
}

/// `shoal stop --interrupt`: cancel the running turns of the agents in the
/// `selected` workspaces, leaving agents without one alone.
pub(super) async fn interrupt(ctx: &Context, selected: Vec<String>) -> Result<i32> {
    let selected = ui::select_workspaces(ctx, selected).await?;
    if crate::env::inherits_scope(&ctx.paths.state) {
        super::stop::ensure_below(ctx, &selected).await?;
    }
    let mut interrupted = Vec::new();
    for selector in selected {
        let workspace = client::inspect(&ctx.paths, selector).await?.workspace;
        interrupted.extend(interrupt_workspace(ctx, &workspace).await?);
    }
    let value = json!({"interrupted": interrupted});
    if interrupted.is_empty() {
        ctx.emit("No running agent turns", value)?;
        return Ok(0);
    }
    let message = interrupted
        .iter()
        .map(|turn| {
            format!(
                "Interrupted the turn of {} in {}",
                turn.agent, turn.workspace
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    ctx.emit_styled(Style::Success, &message, value)?;
    Ok(0)
}

/// Interrupt each agent of `workspace` that runs a turn, recording that
/// the interrupt ended it.
async fn interrupt_workspace(ctx: &Context, workspace: &Workspace) -> Result<Vec<Turn>> {
    let agents = workspace_agents(ctx, workspace).await?;
    let running = agents.iter().filter(|agent| runs_turn(agent.state));
    let settings = settings(ctx, workspace).await?;
    let mut interrupted = Vec::new();
    for agent in running {
        let typed = Typed::of(workspace, agent)?;
        let key = agents::interrupt_key(&settings, typed.name)
            .with_context(|| format!("Shoal knows no key that interrupts {}", typed.name))?;
        terminal::send_keys(typed.place, key).await?;
        request::<AgentStatus>(
            &ctx.paths,
            AgentMethod::InterruptTurn {
                workspace: workspace.id.clone(),
                execution: agent.execution.clone(),
            },
        )
        .await?;
        interrupted.push(typed.turn(workspace));
    }
    Ok(interrupted)
}

/// Whether an agent in `state` runs a turn, which waits for the user while
/// it asks a question or for permission.
fn runs_turn(state: Option<AgentState>) -> bool {
    matches!(state, Some(AgentState::Working | AgentState::Waiting))
}

async fn workspace_agents(ctx: &Context, workspace: &Workspace) -> Result<Vec<RunningAgent>> {
    request(
        &ctx.paths,
        AgentMethod::WorkspaceAgents {
            workspace: workspace.id.clone(),
        },
    )
    .await
}

/// The settings that choose the integration of each agent of `workspace`.
async fn settings(ctx: &Context, workspace: &Workspace) -> Result<Effective> {
    client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await
}

/// An agent Shoal types into, by name, and the place it runs in.
#[derive(Debug)]
struct Typed<'a> {
    name: &'a str,
    place: &'a Origin,
}

impl<'a> Typed<'a> {
    /// `agent` of `workspace`, refusing one that runs in no terminal host.
    fn of(workspace: &Workspace, agent: &'a RunningAgent) -> Result<Self> {
        let name = agent.agent.as_deref().unwrap_or("the agent");
        let place = agent.origin.as_ref().with_context(|| {
            format!(
                "{name} in {} runs in no terminal session Shoal can type into",
                workspace.name
            )
        })?;
        Ok(Self { name, place })
    }

    fn turn(&self, workspace: &Workspace) -> Turn {
        Turn {
            workspace: workspace.name.clone(),
            agent: self.name.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests;
