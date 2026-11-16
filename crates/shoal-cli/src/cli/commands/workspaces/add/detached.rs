//! Starting an addition's agent detached, as scoped callers and
//! ones without a terminal do unless `--here` runs it in place: the daemon
//! starts it outside the caller, so interrupting the caller leaves it
//! running. A scoped caller's preparation queues the agent; the user's is
//! placed once its workspace is set up. A blocked launch queues whole.
use anyhow::{Context as _, Result, ensure};

use super::{
    AddOptions,
    agent::AddSettings,
    finish::{AgentStart, add_planned, execute_add_with_target},
    plan::planned_workspace_name,
    queue::{admit_launch, failing_entry, launch_blockers},
    target::AddTarget,
};
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
use shoal_ids::queue_entry_ids::QueueEntryId;

/// Whether a start's agent, if `agent`, starts detached, unless `--here`
/// asks to run it in place: for a scoped caller, whose own processes it
/// must not be, and for one without a terminal to start it beside. A scoped
/// caller's addition without an agent opens its workspace the detached way
/// too.
pub(super) fn starts_detached(ctx: &Context, agent: bool, options: &AddOptions) -> bool {
    if crate::env::is_scoped() {
        return !(agent && options.here);
    }
    agent && !options.here && !ctx.on_terminal()
}

/// Whether the daemon carries out a start not run `here` whose agent, if
/// any, is `agent`: one for a scoped caller, whose own processes its agent
/// must not be, or without a terminal to start it beside, unless the user's
/// agent runs its own way under `settings`, which starts in this process.
pub(in crate::cli::commands) fn daemon_starts(
    ctx: &Context,
    here: bool,
    agent: Option<&crate::agent::Agent>,
    settings: &Effective,
) -> bool {
    let own_way = !crate::env::is_scoped()
        && agent.is_some_and(|agent| agents::runs_its_own_way(agent, settings));
    leaves_caller(ctx, here) && !own_way
}

/// Whether a start not run `here` leaves this process: for a scoped
/// caller, or for one without a terminal.
pub(in crate::cli::commands) fn leaves_caller(ctx: &Context, here: bool) -> bool {
    !here && (crate::env::is_scoped() || !ctx.on_terminal())
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

/// Refuse, before any work is created, a detached start of `agent` that
/// `--focus` asks to focus, which nothing shows. The daemon starts the
/// agent from the background, which some host must take, unless the user's
/// agent runs its own way.
pub(super) fn ensure_detachable(
    settings: &Effective,
    agent: Option<&crate::agent::Agent>,
    options: &AddOptions,
) -> Result<()> {
    let Some(agent) = agent else {
        return Ok(());
    };
    ensure_unfocused(options)?;
    if !crate::env::is_scoped() && agents::runs_its_own_way(agent, settings) {
        return Ok(());
    }
    crate::terminal::queued::ensure_background(settings)
}

/// Why the daemon's queued start of an agent under `settings` could not
/// run from this terminal, which the launch carries for the daemon to
/// refuse it with once it knows it starts one, as `--focus`, which nothing
/// shows, is refused here.
pub(super) fn unplaceable(
    settings: &Effective,
    agent: Option<&crate::agent::Agent>,
    options: &AddOptions,
) -> Result<Option<String>> {
    if agent.is_some() {
        ensure_unfocused(options)?;
    }
    let refused = crate::terminal::queued::ensure_background(settings).err();
    Ok(refused.map(|error| format!("{error:#}")))
}

/// Refuse `--focus` for an agent that starts detached, with nothing to
/// focus.
fn ensure_unfocused(options: &AddOptions) -> Result<()> {
    ensure!(
        !options.focus,
        "a call without a terminal starts its agent detached, with nothing to focus; leave out --focus, or pass --here to run it in place"
    );
    Ok(())
}

/// Whether the user's detached start of `agent` starts it in this process,
/// which an agent that runs its own way leaves at once.
pub(super) async fn starts_here(
    settings: &AddSettings<'_>,
    agent: Option<&crate::agent::Agent>,
) -> Result<bool> {
    let Some(agent) = agent.filter(|_| !crate::env::is_scoped()) else {
        return Ok(false);
    };
    Ok(agents::runs_its_own_way(agent, settings.get().await?))
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
