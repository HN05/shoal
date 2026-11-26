//! `shoal internal plan`: the plan an agent's plan tool sets.
use anyhow::Result;
use shoal_protocol::{
    progress::{PlanStep, ProgressChange},
    space_names::SpaceSelector,
};

use crate::{agents::integration, ai::IntegrationKind, client::connection::CommandPaths};

/// Report the plan change the hook event on stdin of an agent with
/// `integration` names, for the space `space` names, else the agent's own.
/// An event that changes no plan reports nothing.
pub async fn report(
    paths: &CommandPaths,
    space: Option<SpaceSelector>,
    integration: IntegrationKind,
) -> Result<i32> {
    let event = super::input::event();
    let change = event.and_then(|event| self::integration(integration).plan_change(&event));
    let Some(change) = change.and_then(single_lines) else {
        return Ok(0);
    };
    let space = super::own_space(paths, space).await?;
    crate::client::progress::set(space, change)
        .send(paths)
        .await?;
    Ok(0)
}

/// `change` with each step name a valid line, leaving out the steps that
/// have none, so a plan tool's long or multi-line step is shown, not
/// refused; none for a created step without a name.
fn single_lines(change: ProgressChange) -> Option<ProgressChange> {
    let step = |step: PlanStep| {
        Some(PlanStep {
            name: shoal_protocol::progress::step_name(&step.name)?,
            ..step
        })
    };
    Some(match change {
        ProgressChange::Steps(steps) => {
            ProgressChange::Steps(steps.into_iter().filter_map(step).collect())
        }
        ProgressChange::StepCreated(created) => ProgressChange::StepCreated(step(created)?),
        ProgressChange::StepChanged {
            key,
            name,
            state,
            removed,
        } => ProgressChange::StepChanged {
            key,
            name: name.and_then(|name| shoal_protocol::progress::step_name(&name)),
            state,
            removed,
        },
        other @ (ProgressChange::Status(_)
        | ProgressChange::AddBlocker { .. }
        | ProgressChange::RemoveBlocker { .. }) => other,
    })
}
