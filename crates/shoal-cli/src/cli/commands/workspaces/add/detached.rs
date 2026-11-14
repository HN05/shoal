//! Starting an addition's agent detached, as scoped callers and
//! ones without a terminal do unless `--here` runs it in place: the daemon
//! starts it outside the caller, so interrupting the caller leaves it
//! running. A scoped caller's preparation queues the agent; the user's is
//! placed once its workspace is set up. A blocked launch queues whole.
use anyhow::{Context as _, Result, ensure};

use super::{
    AddOptions,
    agent::AddSettings,
    finish::{AgentStart, add_planned, execute_add_with_target, queued_launch, report_queued_in},
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

/// Whether the plan's agent starts detached, unless `--here` asks to run it
/// in place: for a scoped caller, whose own processes it must not be, and
/// for one without a terminal to start it beside. A scoped caller's
/// addition without an agent opens its workspace the detached way too.
pub(super) fn starts_detached(ctx: &Context, plan: &AddPlan, options: &AddOptions) -> bool {
    let agent = plan.agent_name().is_some();
    if crate::env::is_scoped() {
        return !(agent && options.here);
    }
    agent && !options.here && !ctx.on_terminal()
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
    // A scoped caller's agent in a recorded workspace queues directly, for
    // the daemon to start; the user's is placed as a new workspace's is.
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
    let caller_starts = !crate::env::is_scoped();
    if let Some(queued) = admit_launch(
        ctx,
        &target,
        &mut plan,
        issue.as_ref(),
        (false, blockers),
        caller_starts,
    )
    .await?
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
/// entry. Nothing reopens the workspace here; the start the daemon makes
/// once the entry is admitted does, as a scoped caller's detached start of
/// the worker of a workspace below its own space has it do.
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
/// agent that `--focus` asks to focus, which nothing shows. The daemon starts the agent from the background, which
/// some host must take, unless the user's agent runs its own way.
pub(super) async fn ensure_detachable(
    settings: &AddSettings<'_>,
    plan: &AddPlan,
    options: &AddOptions,
) -> Result<()> {
    if plan.agent_name().is_none() {
        return Ok(());
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
    let opens = entry.opens.as_ref();
    let started =
        crate::queued_starts::start_admitted_launch(&ctx.paths, plan, opens, &name, ctx.json).await;
    failing_entry(ctx, Some(&entry.id), started).await?;
    ctx.emit(&format!("Starting {name} in the background"), &entry)?;
    Ok(0)
}
