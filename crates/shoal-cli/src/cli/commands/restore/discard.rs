//! `shoal restore --discard-agent`: discarding a space's stopped agent, last
//! sessions, stopped commands and failed launch instead of bringing them
//! back, which leaves its files, branch and worktree as they are.
use anyhow::{Result, ensure};

use crate::cli::commands::resume::{self, failed_starts, saved, targets::Selected};
use crate::client::assignments;
use crate::client::output::restore::DiscardedAgents;
use crate::{
    cli::{args::SpaceTargets, context::Context, ui},
    execution::recovery::Saved,
    model::{QueueEntry, Space},
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

/// Discard the agents of the spaces `targets` selects as `shoal resume`
/// selects them, every space's with one for `--all`, only the `execution`
/// one when given.
pub(super) async fn run(
    ctx: &Context,
    targets: SpaceTargets,
    execution: Option<ExecutionId>,
) -> Result<i32> {
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        resume::ensure_queued()?;
    }
    let Some(selected) = Selected::load(ctx, targets).await? else {
        return Ok(0);
    };
    ensure_one_space(&selected, execution.as_ref())?;
    let launches = match execution {
        None => failed_starts::of(ctx, selected.spaces.as_deref()).await?,
        Some(_) => Vec::new(),
    };
    match selected.spaces.as_deref() {
        Some([space]) if launches.is_empty() => discard_one(ctx, space.clone(), execution).await,
        _ => discard_each(ctx, &selected, &launches, execution.as_ref()).await,
    }
}

/// Refuse `--execution`, which selects a stopped execution of one space,
/// unless the `selected` spaces come to one, before anything is discarded.
fn ensure_one_space(selected: &Selected, execution: Option<&ExecutionId>) -> Result<()> {
    let one = matches!(selected.spaces.as_deref(), Some([_]));
    ensure!(
        execution.is_none() || one,
        "--execution selects a stopped execution of one space"
    );
    Ok(())
}

/// Discard the space's stopped agent, last sessions and stopped commands,
/// only `execution`'s when given.
async fn discard_one(
    ctx: &Context,
    space: SpaceSelector,
    execution: Option<ExecutionId>,
) -> Result<i32> {
    let discarded = assignments::discard_stopped_agent(&ctx.paths, space, execution).await?;
    let report = DiscardedAgents {
        discarded: discarded.executions,
        ..DiscardedAgents::default()
    };
    report_discarded(ctx, &report, &[discarded.space.as_str()])?;
    Ok(0)
}

/// Discard the stopped agents and commands of the `selected` spaces, only
/// `execution`'s when given, and the `launches` that failed to start, going
/// on after one fails.
async fn discard_each(
    ctx: &Context,
    selected: &Selected,
    launches: &[QueueEntry],
    execution: Option<&ExecutionId>,
) -> Result<i32> {
    let mut stopped = saved::discardable(ctx, selected, launches).await?;
    if let Some(execution) = execution {
        for (_, saved) in &mut stopped {
            saved.retain_execution(execution, true);
        }
    }
    let mut report = DiscardedAgents::default();
    let mut names = Vec::new();
    for (space, saved) in &stopped {
        match discard_saved(ctx, space, saved).await {
            Ok(executions) => {
                report.discarded.extend(executions);
                names.push(space.name.as_str());
            }
            Err(error) => (report.failed).push(ui::failure(ctx, "discard", &space.name, &error)),
        }
    }
    for launch in launches {
        match discard_launch(ctx, launch).await {
            Ok(entry) => {
                report.launches.push(entry);
                names.push(launch.name.as_str());
            }
            Err(error) => (report.failed).push(ui::failure(ctx, "discard", &launch.name, &error)),
        }
    }
    report_discarded(ctx, &report, &names)?;
    Ok(if report.failed.is_empty() { 0 } else { 1 })
}

/// Discard the stopped records of `space` each by itself, leaving agents a
/// wrapper waits to restore, and return the executions discarded.
async fn discard_saved(ctx: &Context, space: &Space, saved: &Saved) -> Result<Vec<ExecutionId>> {
    let mut ids = Vec::new();
    for execution in saved.execution_ids() {
        let selector = SpaceSelector::id(&space.id);
        let discarded =
            assignments::discard_stopped_agent(&ctx.paths, selector, Some(execution)).await?;
        ids.extend(discarded.executions);
    }
    Ok(ids)
}

/// Drop the failed `launch` from the agent queue.
async fn discard_launch(ctx: &Context, launch: &QueueEntry) -> Result<QueueEntry> {
    crate::client::queue::remove(&ctx.paths, launch.id.to_string()).await
}

/// Report the agents `discarded` of the spaces `names` lists, or that there
/// was none.
fn report_discarded(ctx: &Context, discarded: &DiscardedAgents, names: &[&str]) -> Result<()> {
    ctx.show(discarded, |discarded| match names {
        [] if discarded.failed.is_empty() => {
            outln!("No stopped agents, commands or failed launches")
        }
        [] => {}
        names => outln!("Discarded the agents of {}", names.join(", ")),
    })
}
