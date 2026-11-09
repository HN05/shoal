//! Starting an addition's agent detached, as scoped callers and
//! noninteractive ones do: the daemon starts it outside the caller, so
//! interrupting the caller leaves it running. A scoped caller's preparation
//! queues the agent; the user's is placed once its workspace is set up.
use anyhow::{Context as _, Result, ensure};

use super::{
    AddOptions,
    agent::AddSettings,
    finish::{AgentStart, add_planned, execute_add_with_target},
    handoff::start_queued,
    plan::planned_workspace_name,
    queue::{admit_launch, failing_entry, launch_dependencies},
    target::AddTarget,
};
use crate::client::queue::QueueEntryId;
use crate::{
    cli::{
        agents,
        commands::{issues, queued_agents},
        context::Context,
        terminal,
    },
    config::Effective,
    model::Dependency,
    start_plan::AddPlan,
};

/// Whether the plan's agent starts detached: always for a scoped caller,
/// which `--here` cannot keep in place, and for a noninteractive one, which
/// has no terminal to start it beside, unless `--here` asks to run it in
/// place.
pub(super) fn starts_detached(ctx: &Context, plan: &AddPlan, options: &AddOptions) -> Result<bool> {
    let agent = plan.agent_name().is_some();
    if crate::env::is_scoped() {
        ensure!(
            !(agent && options.here),
            "scoped processes start agents detached; leave out --here"
        );
        return Ok(true);
    }
    Ok(agent && !options.here && !ctx.interactive())
}

/// Open the planned workspace and start its agent detached. A scoped
/// caller's preparation queues the agent. The user's agent is placed once
/// setup ends. A launch of the user's that waits for other work queues
/// whole, as from a terminal.
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
        let start = match crate::env::is_scoped() {
            true => AgentStart::Queued { now: options.now },
            false => AgentStart::Placed { now: options.now },
        };
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
    // An agent that leaves this process at once needs no background host.
    let settings = AddSettings::new(ctx, &target.selector);
    if runs_its_own_way(settings.get().await?, &plan) {
        return execute_add_with_target(ctx, plan, target, issue).await;
    }
    start_admitted(ctx, &target, plan).await
}

/// Refuse, before any work is created, a detached start of the plan's
/// agent that `options` ask to start differently: one a scoped caller
/// starts regardless of the limits or any that `--focus` focuses, which
/// nothing shows. The daemon starts the agent from the background, which
/// some host must take, unless the user's agent runs its own way.
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
        "a call without a terminal or with --json starts its agent detached, with nothing to focus; leave out --focus, or pass --here to run it in place"
    );
    let settings = settings.get().await?;
    if !crate::env::is_scoped() && runs_its_own_way(settings, plan) {
        return Ok(());
    }
    terminal::ensure_background(settings)
}

/// Whether the plan's agent runs its own way under the checkout's
/// `settings`, leaving this process at once, as before its workspace exists
/// only they can tell; the set-up workspace's settings decide a placed agent.
fn runs_its_own_way(settings: &Effective, plan: &AddPlan) -> bool {
    (plan.agent.agent.as_ref()).is_some_and(|agent| agents::runs_its_own_way(agent, settings))
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
    let id = (plan.queue_entry.as_ref().map(QueueEntryId::as_str))
        .context("the daemon admitted a waiting agent without an entry")?;
    // Read before the agent starts and takes the entry.
    let entry = (queued_agents::entries(ctx).await?.into_iter())
        .find(|queued| queued.id == id)
        .context("the admitted agent left the agent queue")?;
    let name = planned_workspace_name(ctx, target, &plan.creation).await?;
    let started = start_queued(ctx, plan, &name).await;
    failing_entry(ctx, Some(&entry.id), started).await?;
    ctx.emit(&format!("Starting {name} in the background"), &entry)?;
    Ok(0)
}
