//! Interrupting an agent's turn without stopping the agent, and continuing
//! the turn an interrupt ended: Shoal types the agent's own interrupt key, or
//! a prompt to continue, into the place it runs in, through that place's
//! terminal host, and the daemon records that the interrupt ended the turn.
//! Every target is resolved before Shoal types into any; a target Shoal then
//! fails to type into does not keep it from the others.
use anyhow::{Context as _, Result};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use crate::client::output::agents::{Agent, Continued, Interrupted};
use crate::{
    cli::{agents, context::Context, ui},
    model::{RunningAgent, Space},
    protocol::ConfigTarget,
    sessions::Origin,
    state::AgentState,
};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// `shoal stop --interrupt`: cancel the running turns of the agents in the
/// `selected` spaces, workspaces or chats, leaving agents without one alone.
pub(super) async fn interrupt(ctx: &Context, selected: Vec<SpaceSelector>) -> Result<i32> {
    let selected = ui::select_spaces(ctx, selected, false).await?;
    let spaces = super::stop::resolve(ctx, selected, false).await?;
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        super::stop::ensure_below(&spaces)?;
    }
    let interruptions = running_turns(ctx, &spaces).await?;
    let mut report = ui::Report::new("interrupt");
    for interruption in &interruptions {
        let target = &interruption.target;
        match interrupt_turn(ctx, interruption).await {
            Ok(()) => report.done(
                ctx,
                &format!("Interrupted the turn of {}", target.described()),
                target.output(),
            ),
            Err(error) => report.failed(ctx, &target.space, &error),
        }
    }
    let summary = if interruptions.is_empty() {
        "No running agent turns"
    } else {
        ""
    };
    report.finish(ctx, summary, |interrupted, failed| Interrupted {
        interrupted,
        failed,
    })
}

/// A running turn and the key that interrupts it.
struct Interruption {
    target: Target,
    key: &'static str,
}

/// The running turns of the agents in `spaces`, refusing them all when
/// Shoal cannot interrupt one.
async fn running_turns(ctx: &Context, spaces: &[Space]) -> Result<Vec<Interruption>> {
    let mut interruptions = Vec::new();
    for space in spaces {
        let agents = agents_in(ctx, space).await?;
        let settings = shoal_workers::settings::of(&ctx.paths, ConfigTarget::space(space)).await?;
        for agent in agents.into_iter().filter(|agent| runs_turn(agent.state)) {
            let target = Target::of(space, agent)?;
            let key = (target.agent.as_ref())
                .and_then(|agent| agents::interrupt_key(&settings, agent))
                .with_context(|| {
                    format!(
                        "Shoal knows no key that interrupts {}",
                        name(target.agent.as_ref())
                    )
                })?;
            interruptions.push(Interruption { target, key });
        }
    }
    Ok(interruptions)
}

/// Type the interrupt key, then record that it ended the turn.
async fn interrupt_turn(ctx: &Context, interruption: &Interruption) -> Result<()> {
    let target = &interruption.target;
    target.report_typing(ctx).await?;
    crate::terminal::send_keys(&target.place, interruption.key).await?;
    let (space, execution) = (target.space_id.clone(), target.execution.clone());
    crate::client::agents::interrupt_turn(&ctx.paths, SpaceSelector::id(&space), execution).await?;
    Ok(())
}

/// The turns an interrupt ended that `shoal resume` continues.
#[derive(Default)]
pub(super) struct Continuation(Vec<Target>);

impl Continuation {
    /// The turns an interrupt ended of the agents in the spaces `selected`
    /// names, each once, by the selector `space` gives each, with those of
    /// `selected` without one.
    pub(super) async fn plan<T>(
        ctx: &Context,
        selected: Vec<T>,
        space: impl Fn(&T) -> SpaceSelector,
    ) -> Result<(Self, Vec<T>)> {
        let spaces = crate::client::spaces::list(&ctx.paths).await?;
        let mut targets = Vec::new();
        let mut rest = Vec::new();
        for item in selected {
            let selector = space(&item);
            let found = (spaces.find(&selector)).with_context(|| format!("no space {selector}"))?;
            let interrupted: Vec<_> = agents_in(ctx, &found)
                .await?
                .into_iter()
                .filter(idles_interrupted)
                .map(|agent| Target::of(&found, agent))
                .collect::<Result<_>>()?;
            if interrupted.is_empty() {
                rest.push(item);
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
        let report = self.submit_each(ctx).await;
        report.finish(ctx, "", |continued, failed| Continued { continued, failed })
    }

    /// Submit the prompt that continues each turn, for a restore that
    /// reports them with what else it restores.
    pub(super) async fn submit_with_others(self, ctx: &Context) -> Continued {
        let (continued, failed) = self.submit_each(ctx).await.into_parts();
        Continued { continued, failed }
    }

    /// Submit the prompt that continues each turn, printing a line for each
    /// as it goes, going on after one fails.
    async fn submit_each(&self, ctx: &Context) -> ui::Report<Agent> {
        let mut report = ui::Report::new("continue");
        for target in &self.0 {
            match target.continue_turn(ctx).await {
                Ok(()) => report.done(
                    ctx,
                    &format!("Continued the turn of {}", target.described()),
                    target.output(),
                ),
                Err(error) => report.failed(ctx, &target.space, &error),
            }
        }
        report
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

/// The connected agents of `space`.
async fn agents_in(ctx: &Context, space: &Space) -> Result<Vec<RunningAgent>> {
    crate::client::agents::running(&ctx.paths, SpaceSelector::id(&space.id)).await
}

/// `agent`'s name, or a stand-in for one whose wrapper is gone.
fn name(agent: Option<&crate::agent::Agent>) -> &str {
    agent.map_or("the agent", |agent| agent.name())
}

/// An agent Shoal types into, by space and name as output shows it, with
/// its execution and the place it runs in.
#[derive(Debug)]
struct Target {
    space: SpaceName,
    agent: Option<crate::agent::Agent>,
    space_id: SpaceId,
    execution: ExecutionId,
    place: Origin,
}

impl Target {
    /// `agent` of `space`, refusing one that runs in no terminal host.
    fn of(space: &Space, agent: RunningAgent) -> Result<Self> {
        let place = agent.origin.with_context(|| {
            format!(
                "{} in {} runs in no terminal session Shoal can type into",
                name(agent.agent.as_ref()),
                space.name
            )
        })?;
        Ok(Self {
            space: space.name.clone(),
            agent: agent.agent,
            space_id: space.id.clone(),
            execution: agent.execution,
            place,
        })
    }

    /// The agent as output names it.
    fn output(&self) -> Agent {
        Agent {
            space: self.space.clone(),
            agent: self.agent.clone(),
        }
    }

    /// The agent and its space, as messages name them.
    fn described(&self) -> String {
        format!("{} in {}", name(self.agent.as_ref()), self.space)
    }

    /// Tell the daemon that Shoal types into the agent's terminal now, so it
    /// does not count the keys as the user's.
    async fn report_typing(&self, ctx: &Context) -> Result<()> {
        let (space, execution) = (SpaceSelector::id(&self.space_id), self.execution.clone());
        crate::client::agents::report_typing(&ctx.paths, space, execution).await
    }

    /// Submit the prompt that continues the agent's interrupted turn.
    async fn continue_turn(&self, ctx: &Context) -> Result<()> {
        self.report_typing(ctx).await?;
        crate::terminal::submit_prompt(&self.place, crate::terminal::CONTINUE_PROMPT).await
    }
}

#[cfg(test)]
mod tests;
