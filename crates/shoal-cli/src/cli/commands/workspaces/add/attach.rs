//! A start whose agent runs in its caller: here, in a focused place it
//! opens beside this terminal, or in this process for an agent that runs
//! its own way. The daemon records the workspace and prepares it; the
//! caller holds the agent's queue entry, shows the preparation, then runs
//! the agent once the queue lets it start, or leaves it to the queue's
//! background start while the limits hold it back.
use anyhow::{Context as _, Result};

mod placement;

pub(super) use placement::{Attach, in_caller};

use super::{
    finish::{finish_add_workspace, report_queued_in},
    waiting::AfterSetup,
};
use crate::{
    cli::{commands::queued_agents, context::Context, terminal},
    client::output::workspaces::StartedBeside,
    client::preparation::Preparing,
    config::Effective,
    model::{QueueEntry, Workspace},
    protocol::operations::{Prepared, StartedWorkspace},
    start_plan::{AddPlan, PlaceHandoff, ResolvedAddAgent},
    state::QueueState,
    terminal::PlaceName,
};
use shoal_protocol::launches::{Launched, Started};
use shoal_protocol::space_names::SpaceSelector;

/// The workspace a launch started its agent in, as a start's workspace: a
/// new or reopened one with its preparation, or an existing one whose
/// worker it queued, with nothing left to prepare.
pub(super) fn workspace_start(launched: Launched) -> Result<StartedWorkspace> {
    let Launched {
        space,
        entry,
        warnings,
        start,
    } = launched;
    let (operation, reused) = match start {
        Started::Preparing { operation, reused } => (operation, reused),
        Started::Worker => (None, true),
        Started::Queued { .. } | Started::Detached(_) => {
            anyhow::bail!("the daemon started the agent in no workspace")
        }
    };
    let workspace =
        (space.into_workspace().ok()).context("the daemon's launch is no workspace's")?;
    Ok(StartedWorkspace {
        operation,
        workspace: Box::new(workspace),
        reused,
        entry,
        warnings,
    })
}

/// Run the agent of the workspace the daemon `started` here, once its
/// preparation ends and the queue lets it start, holding its entry
/// meanwhile. Without an agent, show the preparation and enter the
/// workspace.
pub(super) async fn run(ctx: &Context, started: StartedWorkspace) -> Result<i32> {
    run_named(ctx, started, None).await
}

/// [`run`] in the place opened for it, named as `place_name` says, which
/// its agent keeps once it starts there.
async fn run_named(
    ctx: &Context,
    started: StartedWorkspace,
    place_name: Option<PlaceName>,
) -> Result<i32> {
    let entry = started.entry.clone();
    let _hold = match &entry {
        Some(entry) => Some(queued_agents::Hold::start(&ctx.paths, &entry.id).await),
        None => None,
    };
    let recorded = started.workspace.clone();
    let workspace = match started.operation {
        Some(_) => match follow(ctx, started, entry.as_deref()).await? {
            Some(workspace) => workspace,
            None => return unprepared(ctx, &recorded, entry.is_some()).await,
        },
        // An existing workspace opened without an agent, which the daemon
        // reopens as any start's, verifying its worktree first.
        None if entry.is_none() => {
            // The reopening names its workspace, so it reports nothing.
            if ctx.json {
                ctx.show(&*started.workspace, |_| ())?;
            }
            let plan = AddPlan::opening(&started.workspace);
            return Box::pin(super::finish::execute_add(ctx, plan)).await;
        }
        None => *started.workspace,
    };
    match entry {
        Some(entry) => start_held(ctx, &workspace, &entry, place_name).await,
        None => shell_in(ctx, &workspace).await,
    }
}

/// End the start of `workspace`, whose preparation failed, leaving its
/// agent, if `with_agent`, waiting in the queue for a retried setup, and
/// the place opened for it hosting the workspace meanwhile.
async fn unprepared(ctx: &Context, workspace: &Workspace, with_agent: bool) -> Result<i32> {
    let label = workspace.name.as_str();
    terminal::host_workspace(ctx, workspace, with_agent, label).await?;
    Ok(1)
}

/// Report the prepared `workspace`, opened without an agent, which `--json`
/// reports once prepared, and give it a shell.
async fn shell_in(ctx: &Context, workspace: &Workspace) -> Result<i32> {
    if ctx.json {
        ctx.show(workspace, |_| ())?;
    }
    give_shell(ctx, workspace).await
}

/// Give `workspace` a shell in the place opened for it, which hosts the
/// workspace; the caller's own terminal gets its shell back.
async fn give_shell(ctx: &Context, workspace: &Workspace) -> Result<i32> {
    terminal::host_workspace(ctx, workspace, false, workspace.name.as_str()).await?;
    terminal::open_shell(ctx, &workspace.path).await
}

/// Run the agent of the workspace the daemon `started`, or its shell
/// without one, in a new place beside this terminal, `focused` or in the
/// background, which shows its preparation, where the host under
/// `settings` opens one; otherwise here.
pub(super) async fn run_beside(
    ctx: &Context,
    started: StartedWorkspace,
    settings: &Effective,
    focused: bool,
) -> Result<i32> {
    let entry = started.entry.as_deref();
    if let Some(entry) = entry
        && started.operation.is_none()
        && !claimed(ctx, &started.workspace, entry).await?
    {
        return Ok(0);
    }
    let mut plan = place_plan(&started, entry);
    let placement = match focused {
        true => terminal::Placement::Focus,
        false => terminal::Placement::Background,
    };
    let workspace = &started.workspace;
    if !terminal::start_add(ctx, settings, placement, &mut plan, workspace).await? {
        return run(ctx, started).await;
    }
    let shown = StartedBeside {
        workspace: started.workspace.name.clone(),
        agent: entry.map(|entry| entry.agent.clone()),
    };
    ctx.show(&shown, |_| ())?;
    Ok(0)
}

/// Run, in the place opened for it, the agent `plan` names the entry of,
/// once the preparation it follows ends and the queue lets it start, or
/// the workspace's shell without one.
pub(in crate::cli) async fn run_in_place(ctx: &Context, plan: AddPlan) -> Result<i32> {
    let workspace = SpaceSelector::id(&plan.workspace);
    let workspace = crate::client::workspaces::inspect(&ctx.paths, workspace)
        .await?
        .workspace;
    let entry = match plan.queue_entry {
        Some(entry) => {
            let entries = queued_agents::entries(ctx).await?;
            let entry = (entries.into_iter()).find(|queued| queued.id == entry);
            Some(entry.with_context(|| format!("the agent of {} left the queue", workspace.name))?)
        }
        None => None,
    };
    let started = StartedWorkspace {
        operation: plan.handoff.and_then(|handoff| handoff.preparation),
        workspace: Box::new(workspace),
        reused: false,
        entry: entry.map(Box::new),
        warnings: Vec::new(),
    };
    run_named(ctx, started, plan.place_name).await
}

/// Follow the preparation the daemon `started` for the agent of `entry`,
/// returning the workspace once it is prepared, or `None` when it was not.
async fn follow(
    ctx: &Context,
    started: StartedWorkspace,
    entry: Option<&QueueEntry>,
) -> Result<Option<Workspace>> {
    let preparing = Preparing::follow(&ctx.paths, started).await?;
    // The daemon queued the agent, which waits for a failed setup itself.
    let mut rest = AfterSetup {
        pr: None,
        waiting: None,
    };
    let agent = ResolvedAddAgent {
        agent: entry.map(|entry| entry.agent.clone()),
        model: Default::default(),
    };
    // `--json` reports the workspace once the agent starts, or its waiting
    // entry instead.
    let finished = finish_add_workspace(ctx, preparing, &mut rest, &agent, ctx.json).await?;
    Ok(finished.map(|Prepared { workspace, .. }| *workspace))
}

/// Start the agent of `entry`, which this process holds, here in
/// `workspace` once the queue lets it, else report it waiting there.
async fn start_held(
    ctx: &Context,
    workspace: &Workspace,
    entry: &QueueEntry,
    place_name: Option<PlaceName>,
) -> Result<i32> {
    if !claimed(ctx, workspace, entry).await? {
        return Ok(0);
    }
    if ctx.json {
        ctx.show(workspace, |_| ())?;
    }
    let (_, mut plan) = crate::queued_starts::launch_plan(&ctx.paths, &entry.id).await?;
    if place_name.is_some() {
        plan.place_name = place_name;
    }
    super::finish::execute_add(ctx, plan).await
}

/// Claim the agent of `entry` for this process, reporting it waiting in
/// `workspace` when the queue does not let it start yet.
async fn claimed(ctx: &Context, workspace: &Workspace, entry: &QueueEntry) -> Result<bool> {
    let claimed = crate::client::queue::claim(&ctx.paths, &entry.id).await?;
    if claimed.state == QueueState::Queued {
        report_queued_in(ctx, workspace, &claimed)?;
        return Ok(false);
    }
    Ok(true)
}

/// The plan of the place that runs the agent of `entry`, or the shell of
/// the workspace the daemon `started` without one, after following its
/// preparation, if any. The place reads what the agent starts with once
/// the queue lets it start.
fn place_plan(started: &StartedWorkspace, entry: Option<&QueueEntry>) -> AddPlan {
    let mut plan = AddPlan::opening(&started.workspace);
    // The place takes its name from the workspace's issue, as a template asks.
    plan.issue = (started.workspace.links.issue()).map(|issue| issue.url.clone());
    plan.agent.agent = entry.map(|entry| entry.agent.clone());
    plan.queue_entry = entry.map(|entry| entry.id.clone());
    plan.handoff = Some(PlaceHandoff {
        preparation: started.operation.clone(),
    });
    plan
}
