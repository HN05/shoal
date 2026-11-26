//! A start whose agent runs in its caller: here, in a place beside its
//! terminal, or in this process.
use anyhow::{Context as _, Result};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    protocol::{Launched, Started},
    space_names::SpaceSelector,
};

use super::{Starting, follow_preparation, run::run_entry, shell_in, unprepared};
use crate::{
    agents::HomeRecords,
    client::output::{self, spaces::StartedBeside},
    config::Effective,
    hosting,
    model::{QueueEntry, Space},
    start_plan::{PlaceHandoff, PlacePlan},
    state::QueueState,
    terminal::{PlaceName, Placement},
};

/// Where the agent of a start runs when it runs in its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attach {
    /// In the caller's terminal, or in its process for an agent that runs
    /// its own way, which leaves it at once.
    Here,
    /// In a new place beside the caller's terminal, `focused` or in the
    /// background, else here.
    Beside { focused: bool },
}

/// The space a launch recorded for a caller that runs its agent: the
/// operation preparing it, when it is a workspace that needs one, and its
/// agent's entry, if it starts one.
pub struct Placed {
    pub space: Box<Space>,
    pub operation: Option<String>,
    pub entry: Option<Box<QueueEntry>>,
}

impl Placed {
    /// The space `launched` recorded, with what follows its recording.
    pub fn of(launched: Launched) -> Self {
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

/// Run the agent of the space `placed`, or a workspace's shell without
/// one, in this caller as `attach` says under `settings`.
pub async fn run(
    starting: &impl Starting,
    placed: Placed,
    (attach, settings): (Attach, &Effective),
) -> Result<i32> {
    match attach {
        Attach::Here => Box::pin(run_named(starting, placed, None)).await,
        Attach::Beside { focused } => {
            Box::pin(run_beside(starting, placed, settings, focused)).await
        }
    }
}

/// Run the agent of `placed` here once a workspace's preparation ends and
/// the queue lets it start, holding its entry meanwhile, in the place
/// opened for it, named as `place_name` says, which its agent keeps once
/// it starts there. Without an agent, follow the preparation and enter
/// the workspace.
async fn run_named(
    starting: &impl Starting,
    placed: Placed,
    place_name: Option<PlaceName>,
) -> Result<i32> {
    let Placed {
        space,
        operation,
        entry,
    } = placed;
    let _hold = match &entry {
        Some(entry) => {
            Some(crate::client::queue::Hold::start(starting.caller().paths, &entry.id).await)
        }
        None => None,
    };
    let space = match (operation, space.to_workspace()) {
        (Some(operation), Some(workspace)) => {
            let agent = entry.as_ref().map(|entry| entry.agent.clone());
            match follow_preparation(starting, workspace.clone(), operation, agent).await? {
                Some(prepared) => Box::new(prepared.into()),
                None => {
                    return unprepared(starting, &workspace, entry.is_some()).await;
                }
            }
        }
        // An existing workspace opened without an agent, which the daemon
        // reopens as any start's, verifying its worktree first.
        (None, Some(_)) if entry.is_none() => {
            // The reopening names its workspace, so it reports nothing.
            if starting.json() {
                print_json(&*space)?;
            }
            let plan = PlacePlan::new(&space.id, None);
            return Box::pin(execute_place(starting, plan)).await;
        }
        _ => space,
    };
    match entry {
        Some(entry) => start_held(starting, &space, &entry, place_name).await,
        None => {
            let workspace = (space.to_workspace()).context("a chat starts with its agent")?;
            shell_in(starting, &workspace).await
        }
    }
}

/// Run the agent of `placed`, or a workspace's shell without one, in a new
/// place beside this terminal, `focused` or in the background, which
/// follows a workspace's preparation, where the host under `settings`
/// opens one; otherwise here.
async fn run_beside(
    starting: &impl Starting,
    placed: Placed,
    settings: &Effective,
    focused: bool,
) -> Result<i32> {
    let entry = placed.entry.as_deref();
    if let Some(entry) = entry
        && placed.operation.is_none()
        && !claimed(starting, &placed.space.name, entry).await?
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
        true => Placement::Focus,
        false => Placement::Background,
    };
    let opened = hosting::start_place(
        starting.caller(),
        settings,
        placement,
        &mut plan,
        &placed.space,
    )
    .await;
    if !failing_entry(starting, entry, opened).await? {
        return run_named(starting, placed, None).await;
    }
    let shown = StartedBeside {
        space: placed.space.name.clone(),
        agent: entry.map(|entry| entry.agent.clone()),
    };
    if starting.json() {
        print_json(&shown)?;
    }
    Ok(0)
}

/// `opened`, after failing the starting launch `entry`, if any, with its
/// error, since no place runs its agent.
async fn failing_entry(
    starting: &impl Starting,
    entry: Option<&QueueEntry>,
    opened: Result<bool>,
) -> Result<bool> {
    if let (Some(entry), Err(error)) = (entry, &opened) {
        crate::queued_starts::fail(starting.caller().paths, &entry.id, error).await;
    }
    opened
}

/// Carry out `plan` in the place opened for it, or in no place: follow
/// the preparation it hands over and run its agent once the queue lets it,
/// as a place beside a start's terminal does, or run the agent of the
/// starting launch it names, or else give the workspace it names a shell.
pub async fn execute_place(starting: &impl Starting, plan: PlacePlan) -> Result<i32> {
    if plan.handoff.is_some() {
        return Box::pin(run_in_place(starting, plan)).await;
    }
    if let Some(entry) = &plan.queue_entry {
        return Box::pin(run_entry(starting, entry, plan.place_name)).await;
    }
    shell_in_place(starting, &plan.space, plan.place_name).await
}

/// Give the workspace `space`, which its start's launch reopened, a shell
/// in the place opened for it, named as `place_name` says.
async fn shell_in_place(
    starting: &impl Starting,
    space: &SpaceId,
    place_name: Option<PlaceName>,
) -> Result<i32> {
    let records = crate::homes::records(starting.caller().paths, &SpaceSelector::id(space)).await?;
    let HomeRecords::Workspace(workspace) = &records else {
        anyhow::bail!("a chat starts with its agent");
    };
    super::run::host(starting, &records, place_name, None).await?;
    hosting::open_shell(starting.caller(), &workspace.path).await
}

/// Run, in the place opened for it, the agent `plan` names the entry of,
/// once the preparation it follows ends and the queue lets it start, or
/// the workspace's shell without one.
async fn run_in_place(starting: &impl Starting, plan: PlacePlan) -> Result<i32> {
    let spaces = crate::client::spaces::list(starting.caller().paths).await?;
    let space = (spaces.find(&SpaceSelector::id(&plan.space)))
        .with_context(|| format!("no space {}", plan.space))?;
    let entry = match plan.queue_entry {
        Some(entry) => {
            let entries = crate::client::queue::entries(starting.caller().paths).await?;
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
    run_named(starting, placed, plan.place_name).await
}

/// Start the agent of `entry`, which this process holds, here in `space`
/// once the queue lets it, else report it waiting there.
async fn start_held(
    starting: &impl Starting,
    space: &Space,
    entry: &QueueEntry,
    place_name: Option<PlaceName>,
) -> Result<i32> {
    if !claimed(starting, &space.name, entry).await? {
        return Ok(0);
    }
    if starting.json() {
        print_json(space)?;
    }
    Box::pin(run_entry(starting, &entry.id, place_name)).await
}

/// Claim the agent of `entry` for this process, reporting it waiting in
/// the space `name` when the queue does not let it start yet.
async fn claimed(starting: &impl Starting, name: &str, entry: &QueueEntry) -> Result<bool> {
    let claimed = crate::client::queue::claim(starting.caller().paths, &entry.id).await?;
    if claimed.state == QueueState::Queued {
        print(
            starting,
            &output::queue::queued_in(name, &claimed),
            &claimed,
        )?;
        return Ok(false);
    }
    Ok(true)
}

/// Print `text`, or `value` as JSON where the process prints JSON.
fn print(starting: &impl Starting, text: &str, value: &impl output::Output) -> Result<()> {
    shoal_core::outln!("{}", output::line(starting.json(), text, value)?);
    Ok(())
}

fn print_json(value: &impl output::Output) -> Result<()> {
    shoal_core::outln!("{}", output::json(value)?);
    Ok(())
}
