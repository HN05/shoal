//! Starting a planned addition in another terminal place.
use anyhow::Result;

use super::{AddOptions, agent::AddSettings, plan::planned_workspace_name, target::AddTarget};
use crate::{
    cli::{commands::issues, context::Context, terminal},
    client::output::workspaces::StartedBeside,
    protocol::ConfigTarget,
    start_plan::AddPlan,
};

/// Start `plan` beside this terminal, focused or in the background, unless
/// `options.here` keeps it in place or the call does not start it beside
/// its terminal. Returns whether it left this process, which `--json`
/// reports.
pub(super) async fn start_beside(
    ctx: &Context,
    target: &AddTarget,
    settings: &AddSettings<'_>,
    plan: &mut AddPlan,
    issue: Option<&issues::Issue>,
    options: AddOptions,
) -> Result<bool> {
    if options.here || !options.starts_beside(ctx) {
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

/// Start an admitted queue entry's plan in the background where `shoal start`
/// would have from the terminal that queued it.
pub(in crate::cli) async fn start_queued(
    ctx: &Context,
    mut plan: AddPlan,
    name: &str,
) -> Result<()> {
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Repository(plan.repository.clone()),
    )
    .await?;
    terminal::start_queued(ctx, &settings, &mut plan, name).await
}
