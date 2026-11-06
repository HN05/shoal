//! Starting an addition's agent detached, as scoped callers and callers
//! without a terminal do: the workspace's preparation queues the agent,
//! which the daemon starts outside the caller, so interrupting the caller
//! leaves it running.
use anyhow::{Context as _, Result, ensure};

use super::{
    AddOptions,
    agent::AddSettings,
    finish::{AgentStart, add_planned},
    handoff::start_queued,
    plan::{AddPlan, planned_workspace_name},
    queue::{admit_launch, failing_entry, launch_dependencies},
    target::AddTarget,
};
use crate::{
    cli::{
        agents,
        commands::{agent_queue, issues},
        context::Context,
        terminal,
    },
    model::Dependency,
};

/// Whether the plan's agent starts detached: always for a scoped caller,
/// which `--here` cannot keep in place, and for a noninteractive one, which
/// has no terminal to start it beside, unless `--here` asks to run it in
/// place or the agent leaves the caller at once anyway, as one Happy runs
/// does.
pub(super) async fn starts_detached(
    ctx: &Context,
    settings: &AddSettings<'_>,
    plan: &AddPlan,
    options: &AddOptions,
) -> Result<bool> {
    let agent = plan.agent.agent.as_ref();
    if crate::env::is_scoped() {
        ensure!(
            !(agent.is_some() && options.here),
            "scoped processes start agents detached; leave out --here"
        );
        return Ok(true);
    }
    let Some(agent) = agent.filter(|_| !options.here && !ctx.interactive()) else {
        return Ok(false);
    };
    Ok(!agents::runs_its_own_way(agent, settings.get().await?))
}

/// Open the planned workspace and have its preparation queue the agent. A
/// launch of the user's that waits for other work queues whole instead, as
/// from a terminal, and starts in the background once that work is
/// complete, or now when it already is.
pub(super) async fn add_detached(
    ctx: &Context,
    mut plan: AddPlan,
    target: AddTarget,
    issue: Option<issues::Issue>,
    options: AddOptions,
    after: Vec<Dependency>,
) -> Result<i32> {
    if plan.agent_name().is_none() {
        return add_planned(ctx, plan, target, issue, AgentStart::Here).await;
    }
    let after = launch_dependencies(ctx, &target, issue.as_ref(), options.now, after).await?;
    if after.is_empty() {
        let start = AgentStart::Queued { now: options.now };
        return add_planned(ctx, plan, target, issue, start).await;
    }
    ensure!(
        !crate::env::is_scoped(),
        "scoped processes add a workspace at once, so its agent cannot wait for other work; add it once {} is complete",
        waited_for(&after)
    );
    if let Some(queued) =
        admit_launch(ctx, &target, &mut plan, issue.as_ref(), false, after).await?
    {
        return Ok(queued);
    }
    start_admitted(ctx, &target, plan).await
}

/// Refuse, before any work is created, a detached start of the plan's
/// agent that `options` ask to start differently: one a scoped caller
/// starts regardless of the limits or any that `--focus` focuses, which
/// nothing shows. The daemon starts the agent from the background, which
/// some host must take.
pub(super) async fn ensure_detachable(
    settings: &AddSettings<'_>,
    plan: &AddPlan,
    options: &AddOptions,
) -> Result<()> {
    if plan.agent_name().is_none() {
        return Ok(());
    }
    if crate::env::is_scoped() {
        ensure!(
            !options.now && !options.focus,
            "scoped processes start agents when a slot is free; leave out --now and --focus"
        );
    }
    ensure!(
        !options.focus,
        "without a terminal the agent starts detached, with nothing to focus; leave out --focus, or pass --here to run it in place"
    );
    terminal::ensure_background(settings.get().await?)
}

/// The names of the work `after` lists.
fn waited_for(after: &[Dependency]) -> String {
    let names: Vec<_> = (after.iter())
        .map(|dependency| match dependency {
            Dependency::Work(name) | Dependency::Issue(name) => name.as_str(),
        })
        .collect();
    names.join(", ")
}

/// Start the `plan` the queue admitted at once, since the work it waits for
/// is already complete, in the background as the daemon starts it once
/// that work completes, and report its entry.
async fn start_admitted(ctx: &Context, target: &AddTarget, plan: AddPlan) -> Result<i32> {
    let id = (plan.queue_entry.as_deref())
        .context("the daemon admitted a waiting agent without an entry")?;
    // Read before the agent starts and takes the entry.
    let entry = (agent_queue::entries(ctx).await?.into_iter())
        .find(|queued| queued.id == id)
        .context("the admitted agent left the agent queue")?;
    let name = planned_workspace_name(ctx, target, &plan.creation).await?;
    let started = start_queued(ctx, plan, &name).await;
    failing_entry(ctx, Some(&entry.id), started).await?;
    ctx.emit(&format!("Starting {name} in the background"), &entry)?;
    Ok(0)
}
