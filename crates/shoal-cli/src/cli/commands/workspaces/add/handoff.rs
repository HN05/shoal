//! Starting a planned addition in another terminal place.
use anyhow::Result;

use super::{AddOptions, agent::AddSettings, plan::planned_workspace_name, target::AddTarget};
use crate::{
    cli::{commands::issues, context::Context, terminal},
    client::output::workspaces::StartedBeside,
    start_plan::AddPlan,
};

/// Start `plan` beside this terminal, focused or in the background, unless
/// `options.here` keeps it in place or the call has no terminal to start it
/// beside. Returns whether it left this process, which `--json` reports.
pub(super) async fn start_beside(
    ctx: &Context,
    target: &AddTarget,
    settings: &AddSettings<'_>,
    plan: &mut AddPlan,
    issue: Option<&issues::Issue>,
    options: AddOptions,
) -> Result<bool> {
    if options.here || !ctx.on_terminal() {
        return Ok(false);
    }
    let settings = settings.get().await?;
    let name = planned_workspace_name(ctx, target, &plan.creation).await?;
    if !terminal::start_add(ctx, settings, options.focus, plan, issue, &name).await? {
        return Ok(false);
    }
    let agent = plan.agent_name();
    let started = StartedBeside {
        workspace: name,
        agent,
    };
    ctx.show(&started, |_| ())?;
    Ok(true)
}
