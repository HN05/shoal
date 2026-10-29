//! Interrupting an agent's turn without stopping the agent, and continuing
//! the turn an interrupt ended: Shoal types the agent's own interrupt key, or
//! a prompt to continue, into the place it runs in, through that place's
//! terminal host, and the daemon records that the interrupt ended the turn.
//! Every target is resolved before Shoal types into any; a target Shoal then
//! fails to type into does not keep it from the others.
use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::json;

use crate::{
    cli::{
        agents,
        client::{self, request},
        context::Context,
        terminal, ui,
    },
    config::Effective,
    model::{AgentStatus, RunningAgent, Workspace},
    protocol::{AgentMethod, ConfigTarget},
    sessions::Origin,
    state::AgentState,
};

/// What `shoal resume` submits to an agent whose turn an interrupt ended.
const CONTINUE_PROMPT: &str = "Continue where you left off.";

/// `shoal stop --interrupt`: cancel the running turns of the agents in the
/// `selected` workspaces, leaving agents without one alone.
pub(super) async fn interrupt(ctx: &Context, selected: Vec<String>) -> Result<i32> {
    let selected = ui::select_workspaces(ctx, selected).await?;
    if crate::env::inherits_scope(&ctx.paths.state) {
        super::stop::ensure_below(ctx, &selected).await?;
    }
    let interruptions = running_turns(ctx, selected).await?;
    let mut report = ui::Report::new("interrupted", "interrupt");
    for interruption in &interruptions {
        let target = &interruption.target;
        match interrupt_turn(ctx, interruption).await {
            Ok(()) => report.done(
                ctx,
                &format!("Interrupted the turn of {}", target.described()),
                json!(target),
            ),
            Err(error) => report.failed(ctx, &target.workspace, &error),
        }
    }
    let summary = if interruptions.is_empty() {
        "No running agent turns"
    } else {
        ""
    };
    report.finish(ctx, summary)
}

/// A running turn and the key that interrupts it.
struct Interruption {
    target: Target,
    key: &'static str,
}

/// The running turns of the agents in the workspaces `selected` names,
/// refusing them all when Shoal cannot interrupt one.
async fn running_turns(ctx: &Context, selected: Vec<String>) -> Result<Vec<Interruption>> {
    let mut interruptions = Vec::new();
    for selector in selected {
        let (workspace, agents) = agents_of(ctx, selector).await?;
        let settings = settings(ctx, &workspace).await?;
        for agent in agents.into_iter().filter(|agent| runs_turn(agent.state)) {
            let target = Target::of(&workspace, agent)?;
            let key = agents::interrupt_key(&settings, &target.agent)
                .with_context(|| format!("Shoal knows no key that interrupts {}", target.agent))?;
            interruptions.push(Interruption { target, key });
        }
    }
    Ok(interruptions)
}

/// Type the interrupt key, then record that it ended the turn.
async fn interrupt_turn(ctx: &Context, interruption: &Interruption) -> Result<()> {
    let target = &interruption.target;
    terminal::send_keys(&target.place, interruption.key).await?;
    request::<AgentStatus>(
        &ctx.paths,
        AgentMethod::InterruptTurn {
            workspace: target.workspace_id.clone(),
            execution: target.execution.clone(),
        },
    )
    .await?;
    Ok(())
}

/// The turns an interrupt ended that `shoal resume` continues.
pub(super) struct Continuation(Vec<Target>);

impl Continuation {
    /// The turns an interrupt ended of the agents in the workspaces
    /// `selectors` name, each once, with the selectors of the workspaces
    /// without one.
    pub(super) async fn plan(ctx: &Context, selectors: Vec<String>) -> Result<(Self, Vec<String>)> {
        let mut targets = Vec::new();
        let mut rest = Vec::new();
        for selector in selectors {
            let (workspace, agents) = agents_of(ctx, selector.clone()).await?;
            let interrupted: Vec<_> = agents
                .into_iter()
                .filter(idles_interrupted)
                .map(|agent| Target::of(&workspace, agent))
                .collect::<Result<_>>()?;
            if interrupted.is_empty() {
                rest.push(selector);
            }
            targets.extend(interrupted);
        }
        Ok((Self(targets), rest))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Submit the prompt that continues each turn, returning the exit code.
    pub(super) async fn submit(self, ctx: &Context) -> Result<i32> {
        let mut report = ui::Report::new("continued", "continue");
        for target in &self.0 {
            match terminal::submit_prompt(&target.place, CONTINUE_PROMPT).await {
                Ok(()) => report.done(
                    ctx,
                    &format!("Continued the turn of {}", target.described()),
                    json!(target),
                ),
                Err(error) => report.failed(ctx, &target.workspace, &error),
            }
        }
        report.finish(ctx, "")
    }
}

/// Whether `agent` idles after an interrupt ended its turn.
fn idles_interrupted(agent: &RunningAgent) -> bool {
    agent.interrupted && agent.state == Some(AgentState::Idle)
}

/// Whether an agent in `state` runs a turn, which waits for the user while
/// it asks a question or for permission.
fn runs_turn(state: Option<AgentState>) -> bool {
    matches!(state, Some(AgentState::Working | AgentState::Waiting))
}

/// The workspace `selector` names and its connected agents.
async fn agents_of(ctx: &Context, selector: String) -> Result<(Workspace, Vec<RunningAgent>)> {
    let workspace = client::inspect(&ctx.paths, selector).await?.workspace;
    let agents = request(
        &ctx.paths,
        AgentMethod::WorkspaceAgents {
            workspace: workspace.id.clone(),
        },
    )
    .await?;
    Ok((workspace, agents))
}

/// The settings that choose the integration of each agent of `workspace`.
async fn settings(ctx: &Context, workspace: &Workspace) -> Result<Effective> {
    client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await
}

/// An agent Shoal types into, by workspace and name as output shows it, with
/// its execution and the place it runs in.
#[derive(Debug, Serialize)]
struct Target {
    workspace: String,
    agent: String,
    #[serde(skip)]
    workspace_id: String,
    #[serde(skip)]
    execution: String,
    #[serde(skip)]
    place: Origin,
}

impl Target {
    /// `agent` of `workspace`, refusing one that runs in no terminal host.
    fn of(workspace: &Workspace, agent: RunningAgent) -> Result<Self> {
        let name = agent.agent.unwrap_or_else(|| "the agent".into());
        let place = agent.origin.with_context(|| {
            format!(
                "{name} in {} runs in no terminal session Shoal can type into",
                workspace.name
            )
        })?;
        Ok(Self {
            workspace: workspace.name.clone(),
            agent: name,
            workspace_id: workspace.id.clone(),
            execution: agent.execution,
            place,
        })
    }

    /// The agent and its workspace, as messages name them.
    fn described(&self) -> String {
        format!("{} in {}", self.agent, self.workspace)
    }
}

#[cfg(test)]
mod tests;
