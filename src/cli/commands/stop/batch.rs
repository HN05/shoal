//! Stopping several workspaces at once, reporting each.
use anyhow::Result;
use futures_util::future::join_all;

use crate::client::output::agents::{CancelledRun, StoppedWorkspaces};
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    cli::{context::Context, ui},
    client::processes::Selection,
    execution::recovery,
    model::{StoppedWorkspace, Workspace},
    protocol::QueuedLaunches,
};

use super::{
    cancelled_run_message, continue_hint, other_processes, processes::stop_untracked,
    removed_launches,
};

/// Stop every settled workspace with a running execution, an agent whose
/// restore is queued or requested, or an agent launch waiting to start in
/// it, which stopping cancels; the daemon selects them while no restore or
/// launch can move between the queue and its agent.
pub(super) async fn stop_all(ctx: &Context) -> Result<i32> {
    let running = crate::client::stopping::targets(&ctx.paths).await?;
    if running.is_empty() {
        ctx.emit(
            "No running agents or commands",
            StoppedWorkspaces {
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

/// Stop `workspaces` in parallel, reporting each; one failure does not
/// keep the others running, and an execution Shoal cannot prove ended fails
/// its workspace unless `acknowledge_unproven`. `resume` is what the hint
/// to continue them passes `shoal resume`; empty, it names each one.
pub(super) async fn stop_each(
    ctx: &Context,
    workspaces: &[Workspace],
    resume: &str,
    acknowledge_unproven: bool,
) -> Result<i32> {
    let results = join_all(workspaces.iter().map(|workspace| async {
        let stopped = crate::client::stopping::stop_workspace(
            &ctx.paths,
            SpaceSelector::id(&workspace.id),
            acknowledge_unproven,
            QueuedLaunches::Cancel,
        )
        .await?;
        let other = match stopped.cancelled_run {
            Some(_) => None,
            None => Some(
                stop_untracked(ctx, &SpaceSelector::id(&workspace.id), Selection::Processes)
                    .await?,
            ),
        };
        anyhow::Ok((other, stopped))
    }))
    .await;
    let mut report = ui::Report::new("stop");
    let mut resumable = Vec::new();
    let mut removed = Vec::new();
    for (workspace, result) in workspaces.iter().zip(results) {
        let name = &workspace.name;
        match result {
            Ok((
                _,
                StoppedWorkspace {
                    cancelled_run: Some(run),
                    ..
                },
            )) => report.done(
                ctx,
                &cancelled_run_message(name, &run),
                Outcome::Cancelled(CancelledRun {
                    workspace: name.clone(),
                    run,
                }),
            ),
            Ok((other, StoppedWorkspace { cancelled, .. })) => {
                let mut message = match other.map_or(0, |other| other.processes) {
                    0 => format!("Stopped {name}"),
                    n => format!("Stopped {name} and {}", other_processes(n)),
                };
                if !cancelled.is_empty() {
                    message = format!("{message}. {}", removed_launches(cancelled.len()));
                }
                removed.extend(cancelled);
                report.done(ctx, &message, Outcome::Stopped(name.clone()));
                if has_saved_work(ctx, workspace) {
                    resumable.push(name.as_str());
                }
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
        StoppedWorkspaces {
            stopped,
            failed,
            cancelled: removed,
            cancelled_runs,
        }
    })
}

/// What stopping one of several workspaces did.
enum Outcome {
    Stopped(SpaceName),
    /// A CI job's workspace, whose run the forge was asked to cancel.
    Cancelled(CancelledRun),
}

/// Whether `workspace` saved work for `shoal resume`, which refuses to
/// resume a workspace without any.
fn has_saved_work(ctx: &Context, workspace: &Workspace) -> bool {
    recovery::Saved::load(&ctx.paths, &workspace.id).is_ok_and(|saved| !saved.is_empty())
}
