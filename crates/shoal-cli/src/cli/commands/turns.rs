//! `shoal stop --interrupt`: Shoal types the agent's own interrupt key into the
//! place it runs in. Every target is resolved before Shoal types into any, and
//! one Shoal then fails to type into does not keep it from the others.
use crate::cli::ui::Acting;
use anyhow::{Context as _, Result};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use crate::client::output::agents::{Agent, Interrupted};
use crate::{
    cli::{context::Context, ui},
    model::{RunningAgent, Space},
    protocol::ConfigTarget,
    sessions::Origin,
    state::AgentState,
};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

pub(super) async fn interrupt(ctx: &Context, selected: Vec<SpaceSelector>) -> Result<i32> {
    let selected = ui::select_spaces(ctx, selected, false, &typing()).await?;
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

struct Interruption {
    target: Target,
    key: &'static str,
}

/// All are refused when Shoal cannot interrupt one.
async fn running_turns(ctx: &Context, spaces: &[Space]) -> Result<Vec<Interruption>> {
    let mut interruptions = Vec::new();
    for space in spaces {
        let agents = agents_in(ctx, space).await?;
        let settings = shoal_workers::settings::of(&ctx.paths, ConfigTarget::space(space)).await?;
        for agent in agents.into_iter().filter(|agent| runs_turn(agent.state)) {
            let target = Target::of(space, agent)?;
            let key = (target.agent.as_ref())
                .and_then(|agent| shoal_workers::launch::interrupt_key(&settings, agent))
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

async fn interrupt_turn(ctx: &Context, interruption: &Interruption) -> Result<()> {
    let target = &interruption.target;
    target.report_typing(ctx).await?;
    crate::terminal::send_keys(&target.place, interruption.key).await?;
    let (space, execution) = (target.space_id.clone(), target.execution.clone());
    (crate::client::agents::interrupt_turn(SpaceSelector::id(&space), execution))
        .send(&ctx.paths)
        .await?;
    Ok(())
}

/// A turn waits for the user while it asks a question or for permission.
fn runs_turn(state: Option<AgentState>) -> bool {
    matches!(state, Some(AgentState::Working | AgentState::Waiting))
}

async fn agents_in(ctx: &Context, space: &Space) -> Result<Vec<RunningAgent>> {
    crate::client::agents::running(&ctx.paths, SpaceSelector::id(&space.id)).await
}

fn name(agent: Option<&crate::agent::Agent>) -> &str {
    agent.map_or("the agent", |agent| agent.name())
}

#[derive(Debug)]
struct Target {
    space: SpaceName,
    agent: Option<crate::agent::Agent>,
    space_id: SpaceId,
    execution: ExecutionId,
    /// Where keys for the agent go: its place, or the session nested there.
    place: Origin,
}

impl Target {
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
            place: place.into_typed(),
        })
    }

    fn output(&self) -> Agent {
        Agent {
            space: self.space.clone(),
            agent: self.agent.clone(),
            machine: None,
        }
    }

    fn described(&self) -> String {
        format!("{} in {}", name(self.agent.as_ref()), self.space)
    }

    /// So that the daemon does not count the keys as the user's.
    async fn report_typing(&self, ctx: &Context) -> Result<()> {
        let (space, execution) = (SpaceSelector::id(&self.space_id), self.execution.clone());
        crate::client::agents::report_typing(&ctx.paths, space, execution).await
    }
}

/// Shoal types the interrupt key into a place on this machine.
pub(super) fn typing() -> Acting {
    Acting::here(|space| {
        crate::client::agents::interrupt_turn(space, ExecutionId::from(String::new()))
    })
}

#[cfg(test)]
mod tests;
