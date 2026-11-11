//! Stopping keeps agent sessions and commands, and chats, for
//! `shoal resume`; agents stay down until it continues them. The other
//! processes of a workspace,
//! and with `--processes` or `--leftovers` those of a chat, stop for good,
//! and the agent launches waiting in the queue leave it, having nothing to
//! save.
use anyhow::{Result, ensure};

use crate::client::output::agents::SpaceStopped;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{args::SpaceTargets, commands::queued_agents, context::Context, output::Style, ui},
    client::processes::Selection,
    execution::recovery,
    model::{PresentSpace, StoppedWorkspace, Workspace},
    protocol::{ErrorCode, QueuedLaunches, RemoteError},
};

mod batch;
mod chats;
mod processes;
mod targets;

use batch::{stop_all, stop_each};
pub(super) use processes::processes;
use processes::stop_untracked;
use targets::check_each;
pub(super) use targets::{ensure_below, resolve};

pub(super) async fn run(
    ctx: &Context,
    targets: SpaceTargets,
    yes: bool,
    skip_children: bool,
) -> Result<i32> {
    if targets.all {
        return stop_all(ctx).await;
    }
    // A number always names a space, never a queued launch.
    if let [SpaceSelector::IdOrName(selector)] = targets.space.as_slice()
        && let Some(code) = queued_agents::remove_new(ctx, selector).await?
    {
        return Ok(code);
    }
    let selected = ui::select_spaces(ctx, targets.space, false).await?;
    let selected = resolve(ctx, selected, !skip_children).await?;
    if crate::env::inherits_scope(&ctx.paths.state) {
        ensure_below(&selected)?;
    }
    match selected.as_slice() {
        [PresentSpace::Workspace(workspace)] => stop_one(ctx, workspace, yes).await,
        [PresentSpace::Chat(chat)] => chats::stop_one(ctx, chat).await,
        spaces => {
            check_each(ctx, spaces).await?;
            stop_each(ctx, spaces, "", yes).await
        }
    }
}

/// Stop `workspace`, reporting it alone.
async fn stop_one(ctx: &Context, workspace: &Workspace, yes: bool) -> Result<i32> {
    let selector = SpaceSelector::id(&workspace.id);
    let stopped = stop(ctx, &selector, yes, QueuedLaunches::Cancel).await?;
    if let Some(run) = &stopped.cancelled_run {
        // The job runs until its runner stops it, and its workspace goes then.
        ctx.emit_styled(
            Style::Success,
            &cancelled_run_message(&workspace.name, run),
            SpaceStopped {
                stopped: false,
                saved: false,
                already_stopped: false,
                other_processes: 0,
                cancelled: Vec::new(),
                cancelled_run: Some(run.clone()),
            },
        )?;
        return Ok(0);
    }
    let cancelled = stopped.cancelled;
    let other = stop_untracked(ctx, &selector, Selection::Processes).await?;
    let saved = !recovery::Saved::load(&ctx.paths, &workspace.id)?.is_empty();
    let mut message = "Workspace processes stopped".to_owned();
    if other.processes > 0 {
        message = format!("{message}. Stopped {}", other_processes(other.processes));
    }
    if !cancelled.is_empty() {
        message = format!("{message}. {}", removed_launches(cancelled.len()));
    }
    if saved {
        message = format!("{message}. {}", continue_hint(&workspace.name));
    }
    ctx.emit_styled(
        Style::Success,
        &message,
        SpaceStopped {
            stopped: true,
            saved,
            already_stopped: false,
            other_processes: other.processes,
            cancelled,
            cancelled_run: None,
        },
    )?;
    Ok(0)
}

/// What `shoal stop` says once the forge was asked to cancel `run`, the run
/// of the CI job in `workspace`.
pub(super) fn cancelled_run_message(workspace: &str, run: &str) -> String {
    format!("Asked the forge to cancel {run}; {workspace} goes once its CI job's runner stops it")
}

/// How `shoal stop` names the `count` agent launches it removed from the
/// agent queue.
pub(super) fn removed_launches(count: usize) -> String {
    match count {
        1 => "Removed 1 queued agent from the agent queue".to_owned(),
        n => format!("Removed {n} queued agents from the agent queue"),
    }
}

/// How plain `shoal stop` names the untracked processes it stopped besides
/// the saved agents and commands.
pub(super) fn other_processes(count: usize) -> String {
    match count {
        1 => "1 other process".to_owned(),
        n => format!("{n} other processes"),
    }
}

/// Stop `workspace`, doing with the agent launches waiting to start in it
/// what `launches` says, and return those the stop removed from the queue,
/// or the run the forge was asked to cancel for a CI job's workspace.
/// An execution Shoal cannot prove ended stops only once the user confirms
/// it did, or with `yes`.
pub(super) async fn stop(
    ctx: &Context,
    workspace: &SpaceSelector,
    yes: bool,
    launches: QueuedLaunches,
) -> Result<StoppedWorkspace> {
    let request_stop = |acknowledge_unproven| {
        crate::client::stopping::stop_workspace(
            &ctx.paths,
            workspace.clone(),
            acknowledge_unproven,
            launches,
        )
    };
    let error = match request_stop(yes).await {
        Ok(cancelled) => return Ok(cancelled),
        Err(error) => error,
    };
    let Some(unproven) = error
        .downcast_ref::<RemoteError>()
        .filter(|remote| remote.code == ErrorCode::UnprovenStop)
    else {
        return Err(error);
    };
    let action = format!(
        "{}; check that nothing of it still runs. Treat it as stopped",
        unproven.message
    );
    ensure!(ui::confirm(ctx, &action, "--yes")?, "stop canceled");
    request_stop(true).await
}

/// How to continue what `shoal stop` stopped in `targets`, the arguments of
/// `shoal resume`, and how to take agents down only briefly instead.
pub(super) fn continue_hint(targets: &str) -> String {
    format!(
        "Continue with shoal resume {targets}. To restart agents without stopping them, use shoal restart"
    )
}
