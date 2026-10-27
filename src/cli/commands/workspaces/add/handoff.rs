//! Starting a planned addition in another terminal place.
use anyhow::Result;

use super::{
    AddOptions,
    plan::{AddPlan, planned_workspace_name},
    target::AddTarget,
};
use crate::{
    cli::{client, commands::issues, context::Context, terminal},
    protocol::ConfigTarget,
};

/// Start `plan` beside this terminal, focused or in the background, unless
/// `options.here` keeps it in place. Returns whether it left this process.
pub(super) async fn start_beside(
    ctx: &Context,
    target: &AddTarget,
    plan: &mut AddPlan,
    issue: Option<&issues::Issue>,
    options: AddOptions,
) -> Result<bool> {
    if options.here || !ctx.interactive() {
        return Ok(false);
    }
    let mut settings = client::settings(
        &ctx.paths,
        ConfigTarget::Repository(plan.repository.clone()),
    )
    .await?;
    if options.focus {
        settings.focus = Some(true);
    }
    let name = planned_workspace_name(ctx, target, &plan.creation).await?;
    terminal::start_add(ctx, &settings, plan, issue, &name).await
}

/// Start an admitted queue entry's plan in the background where `shoal add`
/// would have from the terminal that queued it.
pub(in crate::cli) async fn start_queued(
    ctx: &Context,
    mut plan: AddPlan,
    name: &str,
) -> Result<()> {
    let settings = client::settings(
        &ctx.paths,
        ConfigTarget::Repository(plan.repository.clone()),
    )
    .await?;
    terminal::start_queued(ctx, &settings, &mut plan, name).await
}
