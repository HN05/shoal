//! Stopping several spaces at once, reporting each.
use crate::client::space_ids::SpaceId;
use anyhow::Result;
use futures_util::future::join_all;

use crate::client::output::agents::{CancelledRun, StoppedSpaces};
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    cli::{context::Context, ui},
    client::processes::Selection,
    execution::recovery,
    model::{PresentSpace, QueueEntry, StoppedChat, Workspace},
    protocol::QueuedLaunches,
};

use super::{
    cancelled_run_message, chats, continue_hint, other_processes, processes::stop_untracked,
    removed_launches,
};

/// Stop every settled workspace with a running execution, an agent whose
/// restore is queued or requested, or an agent launch waiting to start in
/// it, which stopping cancels, and every running chat; the daemon selects
/// them while no restore or launch can move between the queue and its agent.
pub(super) async fn stop_all(ctx: &Context) -> Result<i32> {
    let running = crate::client::stopping::targets(&ctx.paths).await?;
    if running.is_empty() {
        ctx.emit(
            "No running agents or commands",
            StoppedSpaces {
                stopped: Vec::new(),
                failed: Vec::new(),
                cancelled: Vec::new(),
                cancelled_runs: Vec::new(),
            },
        )?;
        return Ok(0);
    }
    stop_each(ctx, &running, "--all", false).await
}

/// Stop `spaces` in parallel, reporting each; one failure does not keep the
/// others running, and an execution Shoal cannot prove ended fails its
/// workspace unless `acknowledge_unproven`. `resume` is what the hint to
/// continue them passes `shoal resume`; empty, it names each one.
pub(super) async fn stop_each(
    ctx: &Context,
    spaces: &[PresentSpace],
    resume: &str,
    acknowledge_unproven: bool,
) -> Result<i32> {
    let stops = spaces
        .iter()
        .map(|space| stop(ctx, space, acknowledge_unproven));
    let results = join_all(stops).await;
    let mut report = ui::Report::new("stop");
    let mut resumable = Vec::new();
    let mut removed = Vec::new();
    for (space, result) in spaces.iter().zip(results) {
        let name = space.name();
        match result {
            Ok(stop) => {
                let message = stop.message(name);
                if stop.saved(ctx, space.id()) {
                    resumable.push(name.as_str());
                }
                let outcome = match stop {
                    Stop::CancelledRun(run) => Outcome::Cancelled(CancelledRun {
                        workspace: name.clone(),
                        run,
                    }),
                    Stop::Workspace { cancelled, .. } => {
                        removed.extend(cancelled);
                        Outcome::Stopped(name.clone())
                    }
                    Stop::Chat(_) => Outcome::Stopped(name.clone()),
                };
                report.done(ctx, &message, outcome);
            }
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    let summary = match (resumable.is_empty(), resume.is_empty()) {
        (true, _) => String::new(),
        (false, true) => continue_hint(&resumable.join(" ")),
        (false, false) => continue_hint(resume),
    };
    report.finish(ctx, &summary, |outcomes, failed| {
        let (mut stopped, mut cancelled_runs) = (Vec::new(), Vec::new());
        for outcome in outcomes {
            match outcome {
                Outcome::Stopped(name) => stopped.push(name),
                Outcome::Cancelled(run) => cancelled_runs.push(run),
            }
        }
        StoppedSpaces {
            stopped,
            failed,
            cancelled: removed,
            cancelled_runs,
        }
    })
}

/// Stop `space`, a workspace with its untracked processes or a chat.
async fn stop(ctx: &Context, space: &PresentSpace, acknowledge_unproven: bool) -> Result<Stop> {
    match space {
        PresentSpace::Workspace(workspace) => {
            stop_workspace(ctx, workspace, acknowledge_unproven).await
        }
        PresentSpace::Chat(chat) => Ok(Stop::Chat(chats::stop(ctx, chat).await?)),
    }
}

/// Stop `workspace`'s agents and commands, cancelling the launches waiting
/// to start in it, then its untracked processes; a CI job's workspace has
/// its run cancelled instead.
async fn stop_workspace(
    ctx: &Context,
    workspace: &Workspace,
    acknowledge_unproven: bool,
) -> Result<Stop> {
    let selector = SpaceSelector::id(&workspace.id);
    let stopped = crate::client::stopping::stop_workspace(
        &ctx.paths,
        selector.clone(),
        acknowledge_unproven,
        QueuedLaunches::Cancel,
    )
    .await?;
    if let Some(run) = stopped.cancelled_run {
        return Ok(Stop::CancelledRun(run));
    }
    let other = stop_untracked(ctx, &selector, Selection::Processes).await?;
    Ok(Stop::Workspace {
        other_processes: other.processes,
        cancelled: stopped.cancelled,
    })
}

/// What stopping one space did.
enum Stop {
    /// A workspace's agents and commands stopped, with how many untracked
    /// processes, and the launches the stop removed from the queue.
    Workspace {
        other_processes: usize,
        cancelled: Vec<QueueEntry>,
    },
    /// A CI job's workspace, whose run the forge was asked to cancel.
    CancelledRun(String),
    Chat(StoppedChat),
}

impl Stop {
    /// What the report says of the space `name` this stop stopped.
    fn message(&self, name: &SpaceName) -> String {
        match self {
            Self::Workspace {
                other_processes: processes,
                cancelled,
            } => {
                let mut message = match processes {
                    0 => format!("Stopped {name}"),
                    n => format!("Stopped {name} and {}", other_processes(*n)),
                };
                if !cancelled.is_empty() {
                    message = format!("{message}. {}", removed_launches(cancelled.len()));
                }
                message
            }
            Self::CancelledRun(run) => cancelled_run_message(name, run),
            Self::Chat(stopped) => chats::stopped_message(name, *stopped),
        }
    }

    /// Whether this stop of the space `id` saved work for `shoal resume`,
    /// which refuses to resume a workspace without any; a chat stopped
    /// before is left to whoever stopped it.
    fn saved(&self, ctx: &Context, id: &SpaceId) -> bool {
        match self {
            Self::Workspace { .. } => {
                recovery::Saved::load(&ctx.paths, id).is_ok_and(|saved| !saved.is_empty())
            }
            Self::CancelledRun(_) => false,
            Self::Chat(stopped) => !stopped.already_stopped,
        }
    }
}

/// What stopping one of several spaces reports.
enum Outcome {
    Stopped(SpaceName),
    /// A CI job's workspace, whose run the forge was asked to cancel.
    Cancelled(CancelledRun),
}
