//! A start whose agent runs in its caller, whatever its space: here, in a
//! place it opens beside this terminal, or in this process for an agent
//! that runs its own way. The daemon records the space and prepares a
//! workspace; the caller holds the agent's queue entry, follows that
//! preparation, then runs the agent once the queue lets it start, or
//! leaves it to the queue's background start while the limits hold it
//! back.
use anyhow::{Context as _, Result};
use shoal_workers::hosting;

use super::placement::Attach;
use crate::{
    agents::HomeRecords,
    cli::{
        args::internal::PlaceWorkerArgs,
        commands::{links, preparation, queued_agents},
        context::Context,
    },
    client::output::spaces::StartedBeside,
    config::Effective,
    model::{QueueEntry, Space},
    start_plan::{PlaceHandoff, PlacePlan},
    state::QueueState,
    terminal::PlaceName,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::launches::Launch;
use shoal_protocol::protocol::{Launched, Started};
use shoal_protocol::space_names::SpaceSelector;

/// The space a launch recorded for a caller that runs its agent: the
/// operation preparing it, when it is a workspace that needs one, and its
/// agent's entry, if it starts one.
pub(in crate::cli::commands) struct Placed {
    pub space: Box<Space>,
    pub operation: Option<String>,
    pub entry: Option<Box<QueueEntry>>,
}

impl Placed {
    /// The space `launched` recorded, with what follows its recording.
    pub(in crate::cli::commands) fn of(launched: Launched) -> Self {
        let operation = match launched.start {
            Started::Preparing { operation, .. } => operation,
            Started::Worker | Started::Queued { .. } | Started::Detached(_) => None,
        };
        Self {
            space: launched.space,
            operation,
            entry: launched.entry,
        }
    }
}

/// Send `launch` under `settings`, its agent, the one it names or else
/// `agent.default`, run in this caller where `options` and this terminal
/// say, else by the daemon, which for a start on another machine this
/// terminal attaches to where `options` show it here. An `issue_linked`
/// workspace's own issue prompts the agent where the launch names nothing
/// to start on.
pub(in crate::cli::commands) async fn place(
    ctx: &Context,
    mut launch: Launch,
    issue_linked: bool,
    settings: &Effective,
    options: &super::StartOptions,
) -> Result<i32> {
    if super::shown::shown_elsewhere(&launch, options) {
        return super::shown::start(ctx, launch).await;
    }
    let agent = (launch.agent.clone()).or_else(|| settings.agent.default.clone());
    let started = (&launch, issue_linked);
    let attach = Attach::of(ctx, started, agent.as_ref(), settings, options);
    let placed = (attach, agent.as_ref());
    super::place_agent(ctx, (&mut launch, issue_linked), placed, settings, options)?;
    send(
        ctx,
        launch,
        (options.wait, agent),
        attach.zip(Some(settings)),
    )
    .await
}

/// Send `launch`, which `agent` runs if it names one, and report what the
/// daemon started, or run its agent in this caller as `attach` says under
/// the start's settings: a workspace's once its preparation ends, unless
/// it waits for other work or disk space, which leaves the agent to the
/// queue's start. A start that runs nothing here reports a new workspace
/// once it is recorded, or with `wait` once its preparation ends.
pub(in crate::cli::commands) async fn send(
    ctx: &Context,
    launch: Launch,
    (wait, agent): (bool, Option<crate::agent::Agent>),
    attach: Option<(Attach, &Effective)>,
) -> Result<i32> {
    let launched = crate::client::launches::launch(&ctx.paths, launch).await?;
    let Some(attach) = attach else {
        return match &launched.start {
            Started::Queued { .. } | Started::Detached(_) => {
                super::reports::report_started(ctx, launched)
            }
            _ => preparation::report_launched(ctx, launched, (wait, agent)).await,
        };
    };
    match &launched.start {
        Started::Detached(_) => super::reports::report_started(ctx, launched),
        // A preparation that waits for other work or disk space leaves the
        // agent to the queue's start once it is done.
        Started::Preparing {
            operation: None, ..
        } => preparation::report_launched(ctx, launched, (false, agent)).await,
        _ => {
            links::print_warnings(&launched.warnings);
            run(ctx, Placed::of(launched), attach).await
        }
    }
}

/// Run the agent of the space `placed`, or a workspace's shell without
/// one, in this caller as `attach` says under `settings`.
pub(in crate::cli::commands) async fn run(
    ctx: &Context,
    placed: Placed,
    (attach, settings): (Attach, &Effective),
) -> Result<i32> {
    match attach {
        Attach::Here => Box::pin(run_named(ctx, placed, None)).await,
        Attach::Beside { focused } => Box::pin(run_beside(ctx, placed, settings, focused)).await,
    }
}

/// Run the agent of `placed` here once a workspace's preparation ends and
/// the queue lets it start, holding its entry meanwhile, in the place
/// opened for it, named as `place_name` says, which its agent keeps once
/// it starts there. Without an agent, follow the preparation and enter
/// the workspace.
async fn run_named(ctx: &Context, placed: Placed, place_name: Option<PlaceName>) -> Result<i32> {
    let Placed {
        space,
        operation,
        entry,
    } = placed;
    let _hold = match &entry {
        Some(entry) => Some(queued_agents::Hold::start(&ctx.paths, &entry.id).await),
        None => None,
    };
    let space = match (operation, space.to_workspace()) {
        (Some(operation), Some(workspace)) => {
            let agent = entry.as_ref().map(|entry| entry.agent.clone());
            match preparation::follow_preparation(ctx, workspace.clone(), operation, agent).await? {
                Some(prepared) => Box::new(prepared.into()),
                None => return preparation::unprepared(ctx, &workspace, entry.is_some()).await,
            }
        }
        // An existing workspace opened without an agent, which the daemon
        // reopens as any start's, verifying its worktree first.
        (None, Some(_)) if entry.is_none() => {
            // The reopening names its workspace, so it reports nothing.
            if ctx.json {
                ctx.show(&*space, |_| ())?;
            }
            let plan = PlacePlan::new(&space.id, None);
            return Box::pin(execute_place(ctx, plan)).await;
        }
        _ => space,
    };
    match entry {
        Some(entry) => start_held(ctx, &space, &entry, place_name).await,
        None => {
            let workspace = (space.to_workspace()).context("a chat starts with its agent")?;
            preparation::shell_in(ctx, &workspace).await
        }
    }
}

/// Run the agent of `placed`, or a workspace's shell without one, in a new
/// place beside this terminal, `focused` or in the background, which
/// follows a workspace's preparation, where the host under `settings`
/// opens one; otherwise here.
async fn run_beside(
    ctx: &Context,
    placed: Placed,
    settings: &Effective,
    focused: bool,
) -> Result<i32> {
    let entry = placed.entry.as_deref();
    if let Some(entry) = entry
        && placed.operation.is_none()
        && !claimed(ctx, &placed.space.name, entry).await?
    {
        return Ok(0);
    }
    let mut plan = PlacePlan {
        handoff: Some(PlaceHandoff {
            preparation: placed.operation.clone(),
        }),
        ..PlacePlan::new(&placed.space.id, entry.map(|entry| &entry.id))
    };
    let placement = match focused {
        true => crate::terminal::Placement::Focus,
        false => crate::terminal::Placement::Background,
    };
    let opened =
        hosting::start_place(ctx.caller(), settings, placement, &mut plan, &placed.space).await;
    if !failing_entry(ctx, entry, opened).await? {
        return run_named(ctx, placed, None).await;
    }
    let shown = StartedBeside {
        space: placed.space.name.clone(),
        agent: entry.map(|entry| entry.agent.clone()),
    };
    ctx.show(&shown, |_| ())?;
    Ok(0)
}

/// `opened`, after failing the starting launch `entry`, if any, with its
/// error, since no place runs its agent.
async fn failing_entry(
    ctx: &Context,
    entry: Option<&QueueEntry>,
    opened: Result<bool>,
) -> Result<bool> {
    if let (Some(entry), Err(error)) = (entry, &opened) {
        crate::queued_starts::fail(&ctx.paths, &entry.id, error).await;
    }
    opened
}

/// The place worker: carry out the plan `args` give in the place of the
/// host they name that this process runs in, or in no place without one.
pub(in crate::cli) async fn place_worker(mut ctx: Context, args: PlaceWorkerArgs) -> Result<i32> {
    let plan = serde_json::from_str(&args.plan).context("invalid place plan")?;
    if let Some(host) = args.host {
        ctx.place = Some(hosting::Place::here(
            host,
            args.close_when_done,
            args.focused,
        )?);
    }
    execute_place(&ctx, plan).await
}

/// Carry out `plan` in the place opened for it, or in no place: follow
/// the preparation it hands over and run its agent once the queue lets it,
/// as a place beside a start's terminal does, or run the agent of the
/// starting launch it names, or else give the workspace it names a shell.
async fn execute_place(ctx: &Context, plan: PlacePlan) -> Result<i32> {
    if plan.handoff.is_some() {
        return Box::pin(run_in_place(ctx, plan)).await;
    }
    if let Some(entry) = &plan.queue_entry {
        return Box::pin(super::run_entry(ctx, entry, plan.place_name)).await;
    }
    shell_in_place(ctx, &plan.space, plan.place_name).await
}

/// Give the workspace `space`, which its start's launch reopened, a shell
/// in the place opened for it, named as `place_name` says.
async fn shell_in_place(
    ctx: &Context,
    space: &SpaceId,
    place_name: Option<PlaceName>,
) -> Result<i32> {
    let records = shoal_workers::homes::records(&ctx.paths, &SpaceSelector::id(space)).await?;
    let HomeRecords::Workspace(workspace) = &records else {
        anyhow::bail!("a chat starts with its agent");
    };
    super::run::host(ctx, &records, place_name, None).await?;
    hosting::open_shell(ctx.caller(), &workspace.path).await
}

/// Run, in the place opened for it, the agent `plan` names the entry of,
/// once the preparation it follows ends and the queue lets it start, or
/// the workspace's shell without one.
async fn run_in_place(ctx: &Context, plan: PlacePlan) -> Result<i32> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let space = (spaces.find(&SpaceSelector::id(&plan.space)))
        .with_context(|| format!("no space {}", plan.space))?;
    let entry = match plan.queue_entry {
        Some(entry) => {
            let entries = queued_agents::entries(ctx).await?;
            let entry = (entries.into_iter()).find(|queued| queued.id == entry);
            Some(entry.with_context(|| format!("the agent of {} left the queue", space.name))?)
        }
        None => None,
    };
    let placed = Placed {
        space: Box::new(space),
        operation: plan.handoff.and_then(|handoff| handoff.preparation),
        entry: entry.map(Box::new),
    };
    run_named(ctx, placed, plan.place_name).await
}

/// Start the agent of `entry`, which this process holds, here in `space`
/// once the queue lets it, else report it waiting there.
async fn start_held(
    ctx: &Context,
    space: &Space,
    entry: &QueueEntry,
    place_name: Option<PlaceName>,
) -> Result<i32> {
    if !claimed(ctx, &space.name, entry).await? {
        return Ok(0);
    }
    if ctx.json {
        ctx.show(space, |_| ())?;
    }
    Box::pin(super::run_entry(ctx, &entry.id, place_name)).await
}

/// Claim the agent of `entry` for this process, reporting it waiting in
/// the space `name` when the queue does not let it start yet.
async fn claimed(ctx: &Context, name: &str, entry: &QueueEntry) -> Result<bool> {
    let claimed = crate::client::queue::claim(&ctx.paths, &entry.id).await?;
    if claimed.state == QueueState::Queued {
        super::reports::report_queued_in(ctx, name, &claimed)?;
        return Ok(false);
    }
    Ok(true)
}
