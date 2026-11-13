//! Starting an addition's agent detached, as scoped callers and
//! ones without a terminal do: the daemon starts it outside the caller, so
//! interrupting the caller leaves it running. A scoped caller's preparation
//! queues the agent; the user's is placed once its workspace is set up.
//! A blocked launch queues whole.
use anyhow::{Context as _, Result, ensure};

use super::{
    AddOptions,
    agent::AddSettings,
    finish::{AgentStart, add_planned, execute_add_with_target, queued_launch, report_queued_in},
    handoff::start_queued,
    plan::planned_workspace_name,
    queue::{admit_launch, failing_entry, launch_blockers},
    target::AddTarget,
};
use crate::client::queue::{QueueEntryId, WorkerStart};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        agents,
        commands::{issues, queued_agents},
        context::Context,
    },
    config::Effective,
    model::Blocker,
    start_plan::AddPlan,
};

/// Whether the plan's agent starts detached: always for a scoped caller,
/// which `--here` cannot keep in place, and for one without a terminal to
/// start it beside, unless `--here` asks to run it in place.
pub(super) fn starts_detached(ctx: &Context, plan: &AddPlan, options: &AddOptions) -> Result<bool> {
    let agent = plan.agent_name().is_some();
    if crate::env::is_scoped() {
        ensure!(
            !(agent && options.here),
            "scoped processes start agents detached; leave out --here"
        );
        return Ok(true);
    }
    Ok(agent && !options.here && !ctx.on_terminal())
}

/// Open the planned workspace and start its agent detached. A scoped
/// caller's preparation queues the agent. The user's agent is placed once
/// setup ends. A blocked launch queues whole, as from a terminal.
pub(super) async fn add_detached(
    ctx: &Context,
    mut plan: AddPlan,
    target: AddTarget,
    issue: Option<issues::Issue>,
    options: AddOptions,
    blockers: Vec<Blocker>,
) -> Result<i32> {
    if plan.agent_name().is_none() {
        return add_planned(ctx, plan, target, issue, AgentStart::Here).await;
    }
    let blockers = launch_blockers(ctx, &target, issue.as_ref(), options.now, blockers).await?;
    // A scoped caller may not reopen a recorded workspace, so its agent there
    // queues directly; the user's is placed as a new workspace's is.
    if plan.workspace.is_some() && crate::env::is_scoped() {
        return queue_in_existing(ctx, plan, issue.as_ref(), (options.now, blockers)).await;
    }
    if blockers.is_empty() {
        let start = match crate::env::is_scoped() {
            true => AgentStart::Queued { now: options.now },
            false => AgentStart::Placed { now: options.now },
        };
        return add_planned(ctx, plan, target, issue, start).await;
    }
    // The daemon starts a scoped caller's launch itself, in its scope.
    if let Some(queued) =
        admit_launch(ctx, &target, &mut plan, issue.as_ref(), false, blockers).await?
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

/// Queue the agent of the existing workspace `plan` opens, which the daemon
/// starts detached once admitted, at once regardless of the limits when
/// `now` and once what `blockers` names clears, and report the workspace and the
/// entry. Nothing reopens the workspace here, so a scoped caller, which may
/// not, starts the worker of a workspace below its own space this way.
async fn queue_in_existing(
    ctx: &Context,
    plan: AddPlan,
    issue: Option<&issues::Issue>,
    (now, blockers): (bool, Vec<Blocker>),
) -> Result<i32> {
    let workspace = plan
        .workspace
        .clone()
        .context("the plan opens no workspace")?;
    let told = (&plan.prompt, plan.args.as_slice(), &plan.fork);
    let launch = queued_launch(&plan.agent, issue, (&plan.mode, plan.narrowing), told, now)?
        .context("the plan starts no agent")?;
    let start = WorkerStart {
        workspace: SpaceSelector::Id(workspace.clone()),
        launch: (*launch).into(),
        blockers,
        pr: None,
    };
    let entry = crate::client::queue::start_agent(&ctx.paths, start).await?;
    let workspace = crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::Id(workspace))
        .await?
        .workspace;
    ctx.show(&workspace, |_| ())?;
    report_queued_in(ctx, &workspace, &entry)
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
        "a call without a terminal starts its agent detached, with nothing to focus; leave out --focus, or pass --here to run it in place"
    );
    let settings = settings.get().await?;
    if !crate::env::is_scoped() && runs_its_own_way(settings, plan) {
        return Ok(());
    }
    crate::terminal::queued::ensure_background(settings)
}

/// Whether the plan's agent runs its own way under the checkout's
/// `settings`, leaving this process at once, as before its workspace exists
/// only they can tell; the set-up workspace's settings decide a placed agent.
fn runs_its_own_way(settings: &Effective, plan: &AddPlan) -> bool {
    (plan.agent.agent.as_ref()).is_some_and(|agent| agents::runs_its_own_way(agent, settings))
}

/// Start the `plan` the queue admitted at once, since its blockers already
/// cleared, in the background as the daemon starts it once they clear, and
/// report its entry.
async fn start_admitted(ctx: &Context, target: &AddTarget, plan: AddPlan) -> Result<i32> {
    let id = (plan.queue_entry.as_ref().map(QueueEntryId::as_str))
        .context("the daemon admitted a waiting agent without an entry")?;
    // Read before the agent starts and takes the entry.
    let entry = (queued_agents::entries(ctx).await?.into_iter())
        .find(|queued| queued.id == id)
        .context("the admitted agent left the agent queue")?;
    let name = planned_workspace_name(ctx, target, &plan.creation).await?;
    let started = start_queued(ctx, plan, entry.opens.as_ref(), &name).await;
    failing_entry(ctx, Some(&entry.id), started).await?;
    ctx.emit(&format!("Starting {name} in the background"), &entry)?;
    Ok(0)
}
