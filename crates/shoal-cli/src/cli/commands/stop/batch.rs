//! Stopping several spaces at once, reporting each.
use anyhow::Result;
use futures_util::future::join_all;

use crate::client::output::agents::{CancelledRun, StoppedSpaces};
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    cli::{context::Context, ui},
    model::{PresentSpace, QueueEntry},
    protocol::QueuedLaunches,
};

use super::{
    cancelled_run_message, continue_hint, other_processes, other_processes_stop, removed_launches,
    stop_confirming,
};

/// Stop every settled workspace and every chat with a running execution or
/// an agent whose restore is queued or requested, and every workspace with
/// an agent launch waiting to start in it, which stopping cancels; the
/// daemon selects them while no restore or launch can move between the
/// queue and its agent.
pub(super) async fn stop_all(ctx: &Context) -> Result<i32> {
    let running = crate::client::stopping::targets(&ctx.paths).await?;
    // A scoped process that cleared its token gets every space from the
    // daemon, its own among them.
    if crate::env::inherits_scope(&ctx.paths.state) {
        super::ensure_below(&running)?;
    }
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
/// workspace unless `acknowledge_unproven`. `restore` is what the hint to
/// continue them passes `shoal restore`; empty, it names each one.
pub(super) async fn stop_each(
    ctx: &Context,
    spaces: &[PresentSpace],
    restore: &str,
    acknowledge_unproven: bool,
) -> Result<i32> {
    let stops = spaces
        .iter()
        .map(|space| stop(ctx, space, acknowledge_unproven));
    let results = join_all(stops).await;
    let mut report = ui::Report::new("stop");
    let mut restorable = Vec::new();
    let mut removed = Vec::new();
    for (space, result) in spaces.iter().zip(results) {
        let name = space.name();
        match result {
            Ok(stop) => {
                let message = stop.message(name);
                if stop.saved() {
                    restorable.push(space.number().to_string());
                }
                let outcome = match stop {
                    Stop::CancelledRun(run) => Outcome::Cancelled(CancelledRun {
                        workspace: name.clone(),
                        run,
                    }),
                    Stop::Space { cancelled, .. } => {
                        removed.extend(cancelled);
                        Outcome::Stopped(name.clone())
                    }
                };
                report.done(ctx, &message, outcome);
            }
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    let summary = match (restorable.is_empty(), restore.is_empty()) {
        (true, _) => String::new(),
        (false, true) => continue_hint(&restorable.join(" ")),
        (false, false) => continue_hint(restore),
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

/// Stop the agents and commands of `space`, cancelling the launches
/// waiting to start in it, then its untracked processes; a CI job's
/// workspace has its run cancelled instead.
async fn stop(ctx: &Context, space: &PresentSpace, acknowledge_unproven: bool) -> Result<Stop> {
    let stopped = stop_confirming(
        ctx,
        &SpaceSelector::id(space.id()),
        QueuedLaunches::Cancel,
        other_processes_stop(),
        |_| Ok(acknowledge_unproven),
    )
    .await?;
    if let Some(run) = stopped.cancelled_run {
        return Ok(Stop::CancelledRun(run));
    }
    Ok(Stop::Space {
        other_processes: stopped.other_processes,
        cancelled: stopped.cancelled,
        saved: stopped.saved,
    })
}

/// What stopping one space did.
enum Stop {
    /// A space's agents and commands stopped, with how many untracked
    /// processes, the launches the stop removed from the queue, and whether
    /// it saved work for `shoal restore`.
    Space {
        other_processes: usize,
        cancelled: Vec<QueueEntry>,
        saved: bool,
    },
    /// A CI job's workspace, whose run the forge was asked to cancel.
    CancelledRun(String),
}

impl Stop {
    /// What the report says of the space `name` this stop stopped.
    fn message(&self, name: &SpaceName) -> String {
        match self {
            Self::Space {
                other_processes: processes,
                cancelled,
                ..
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
        }
    }

    /// Whether this stop saved work for `shoal restore`, which refuses to
    /// resume a space without any.
    fn saved(&self) -> bool {
        match self {
            Self::Space { saved, .. } => *saved,
            Self::CancelledRun(_) => false,
        }
    }
}

/// What stopping one of several spaces reports.
enum Outcome {
    Stopped(SpaceName),
    /// A CI job's workspace, whose run the forge was asked to cancel.
    Cancelled(CancelledRun),
}
