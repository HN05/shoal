//! Starting a planned addition in another terminal place.
use anyhow::Result;

use super::{AddOptions, agent::AddSettings, plan::planned_workspace_name, target::AddTarget};
use crate::client::{space_ids::SpaceId, space_names::SpaceSelector};
use crate::{
    cli::{agents, commands::issues, context::Context, terminal},
    client::output::workspaces::StartedBeside,
    protocol::ConfigTarget,
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

/// Start an admitted queue entry's plan in the background where `shoal start`
/// would have from the terminal that queued it, or here for an agent that
/// runs its own way and so leaves this process at once, such as Happy's,
/// which needs no terminal host. The settings of the existing workspace
/// the entry `opens`, its worktree's file included, decide where, or else
/// the repository's.
pub(in crate::cli) async fn start_queued(
    ctx: &Context,
    mut plan: AddPlan,
    opens: Option<&SpaceId>,
    name: &str,
) -> Result<()> {
    let target = match opens {
        Some(workspace) => ConfigTarget::Space(SpaceSelector::id(workspace)),
        None => ConfigTarget::Repository(plan.repository.clone()),
    };
    let settings = crate::config::settings(&ctx.paths, target).await?;
    let own_way =
        (plan.agent_name()).is_some_and(|agent| agents::runs_its_own_way(&agent, &settings));
    if own_way {
        return super::execute_add(ctx, plan).await.map(drop);
    }
    crate::terminal::queued::start_add(&ctx.paths, &settings, &mut plan, name).await
}
