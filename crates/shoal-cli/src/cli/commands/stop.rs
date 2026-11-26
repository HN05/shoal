//! `shoal stop`.
use anyhow::{Result, ensure};

use crate::client::output::agents::SpaceStopped;
use crate::{
    cli::{
        args::lifecycle::StopArgs,
        commands::{machines, queued_agents, space_selection},
        context::Context,
        output::Style,
        ui::{self, Acting},
    },
    model::{Space, StoppedSpace},
    protocol::{ErrorCode, QueuedLaunches, RemoteError, UntrackedProcesses},
};
use shoal_protocol::model::Located;
use shoal_protocol::space_names::SpaceSelector;

mod batch;
mod elsewhere;
mod processes;
mod targets;

use batch::{stop_all, stop_each};
pub(super) use processes::{processes, stopping_processes};
use targets::check_each;
pub(super) use targets::{ensure_below, resolve};

pub(super) async fn run(ctx: &Context, args: StopArgs) -> Result<i32> {
    let StopArgs {
        targets,
        yes,
        skip_children,
        ..
    } = args;
    if targets.all {
        return stop_all(ctx).await;
    }
    // A number always names a space, never a queued launch.
    if let [SpaceSelector::Id(selector)] = targets.space.as_slice()
        && let Some(code) = queued_agents::remove_new(ctx, selector.as_str()).await?
    {
        return Ok(code);
    }
    // A process of a space that cleared its token is still that space's.
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        targets::ensure_scoped()?;
    }
    // Every target is found, wherever it runs, and checked before any
    // stops.
    let located = match targets.machines.is_empty() {
        true => space_selection::located(ctx, targets, !skip_children, &stopping()).await?,
        false => targets::on_machines(ctx, targets).await?,
    };
    if located.is_empty() {
        return batch::nothing_to_stop(ctx);
    }
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        ensure_below(&targets::here(&located))?;
    }
    let checked = targets::checked(&located);
    if checked.len() > 1 {
        targets::check_targets(ctx, checked).await?;
    }
    stop_located(ctx, located, yes).await
}

/// Each space goes after the one above it. The hub's held stops are reported
/// once per offline machine.
async fn stop_located(ctx: &Context, located: Vec<Located>, yes: bool) -> Result<i32> {
    let mut code = 0;
    let mut here = Vec::new();
    let mut held = Vec::new();
    for located in located {
        match located {
            Located::Here(space) => here.push(*space),
            Located::Elsewhere { space, .. } => {
                code = code.max(stop_here(ctx, &std::mem::take(&mut here), yes).await?);
                held.extend(elsewhere::stop_one(ctx, &space, yes).await?);
            }
        }
    }
    code = code.max(stop_here(ctx, &here, yes).await?);
    machines::queued_each(ctx, &held, "stops")?;
    Ok(code)
}

async fn stop_here(ctx: &Context, spaces: &[Space], yes: bool) -> Result<i32> {
    match spaces {
        [] => Ok(0),
        [space] => stop_one(ctx, space, yes).await,
        spaces => stop_each(ctx, spaces, "", yes).await,
    }
}

async fn stop_one(ctx: &Context, space: &Space, yes: bool) -> Result<i32> {
    let selector = SpaceSelector::id(&space.id);
    let launches = QueuedLaunches::Cancel;
    let stopped = stop(ctx, &selector, yes, launches, other_processes_stop()).await?;
    if let Some(run) = &stopped.cancelled_run {
        // The job runs until its runner stops it, and its workspace goes then.
        ctx.emit_styled(
            Style::Success,
            &cancelled_run_message(&space.name, run),
            SpaceStopped {
                stopped: false,
                saved: false,
                other_processes: 0,
                cancelled: Vec::new(),
                cancelled_run: Some(run.clone()),
            },
        )?;
        return Ok(0);
    }
    let StoppedSpace {
        cancelled,
        other_processes: processes,
        saved,
        ..
    } = stopped;
    let mut message = match space.kind() {
        crate::model::SpaceKind::Workspace => "Workspace processes stopped".to_owned(),
        crate::model::SpaceKind::Chat => "Chat processes stopped".to_owned(),
    };
    if processes > 0 {
        message = format!("{message}. Stopped {}", other_processes(processes));
    }
    if !cancelled.is_empty() {
        message = format!("{message}. {}", removed_launches(cancelled.len()));
    }
    if saved {
        message = format!("{message}. {}", continue_hint(&space.number.to_string()));
    }
    ctx.emit_styled(
        Style::Success,
        &message,
        SpaceStopped {
            stopped: true,
            saved,
            other_processes: processes,
            cancelled,
            cancelled_run: None,
        },
    )?;
    Ok(0)
}

pub(super) fn cancelled_run_message(workspace: &str, run: &str) -> String {
    format!("Asked the forge to cancel {run}; {workspace} goes once its CI job's runner stops it")
}

pub(super) fn removed_launches(count: usize) -> String {
    match count {
        1 => "Removed 1 queued agent from the agent queue".to_owned(),
        n => format!("Removed {n} queued agents from the agent queue"),
    }
}

pub(super) fn other_processes(count: usize) -> String {
    match count {
        1 => "1 other process".to_owned(),
        n => format!("{n} other processes"),
    }
}

pub(super) fn other_processes_stop() -> UntrackedProcesses {
    UntrackedProcesses::Stop {
        requester: Some(std::process::id()),
    }
}

/// An execution Shoal cannot prove ended stops only once the user confirms it
/// did, or with `yes`.
pub(super) async fn stop(
    ctx: &Context,
    space: &SpaceSelector,
    yes: bool,
    launches: QueuedLaunches,
    processes: UntrackedProcesses,
) -> Result<StoppedSpace> {
    let confirm = |unproven: &RemoteError| {
        if !yes {
            let action = format!(
                "{}; check that nothing of it still runs. Treat it as stopped",
                unproven.message
            );
            ensure!(ui::confirm(ctx, &action, "--yes")?, "stop canceled");
        }
        Ok(true)
    };
    stop_confirming(ctx, space, launches, processes, confirm).await
}

/// A stop the daemon can prove never carries an acknowledgement.
pub(super) async fn stop_confirming(
    ctx: &Context,
    space: &SpaceSelector,
    launches: QueuedLaunches,
    processes: UntrackedProcesses,
    confirm: impl FnOnce(&RemoteError) -> Result<bool>,
) -> Result<StoppedSpace> {
    let request_stop = |acknowledge_unproven| {
        let stop = crate::client::stopping::stop_space(
            space.clone(),
            acknowledge_unproven,
            launches,
            processes,
        );
        stop.send(&ctx.paths)
    };
    let error = match request_stop(false).await {
        Ok(cancelled) => return Ok(cancelled),
        Err(error) => error,
    };
    let Some(unproven) = error
        .downcast_ref::<RemoteError>()
        .filter(|remote| remote.code == ErrorCode::UnprovenStop)
    else {
        return Err(error);
    };
    if !confirm(unproven)? {
        return Err(error);
    }
    request_stop(true).await
}

pub(super) fn continue_hint(targets: &str) -> String {
    format!(
        "Continue with shoal resume {targets}. To restart agents without stopping them, use shoal restart"
    )
}

pub(super) fn stopping() -> Acting {
    Acting::request(|space| {
        let processes = UntrackedProcesses::Stop { requester: None };
        crate::client::stopping::stop_space(space, false, QueuedLaunches::Cancel, processes)
    })
}
