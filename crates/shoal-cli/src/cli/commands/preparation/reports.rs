//! What a start reports of the workspace a launch recorded, and how a
//! creation without an agent enters it.
use anyhow::Result;

use super::{finish::finish_workspace, prepared};
use crate::{
    cli::{
        commands::{
            links, queued_agents,
            start::{self, Attach, StartOptions},
        },
        context::Context,
        output::{Palette, Style},
    },
    model::QueueEntry,
    protocol::{
        ConfigTarget,
        operations::{Prepared, StartedWorkspace},
    },
    state::QueueState,
};
use shoal_protocol::launches::Launch;
use shoal_protocol::protocol::{Launched, Started};
use shoal_protocol::space_names::SpaceSelector;

/// Enter the workspace a creation `started` without an agent where a start
/// without one enters it from the user's terminal: here, or in a place
/// beside it, its shell opened once its preparation ends. `None` where the
/// caller follows the preparation itself, as an agent's creation does.
pub(in crate::cli::commands) async fn enter_created(
    ctx: &Context,
    launch: &Launch,
    started: &StartedWorkspace,
) -> Result<Option<i32>> {
    // A new workspace's worktree may not exist yet, so its repository's
    // settings place it, as they place a start's.
    let workspace = &started.workspace;
    let target = match started.reused {
        true => ConfigTarget::Space(SpaceSelector::id(&workspace.id)),
        false => ConfigTarget::Repository(workspace.repository().into()),
    };
    let settings = &shoal_workers::settings::of(&ctx.paths, target).await?;
    let options = StartOptions::default();
    let Some(attach) = Attach::of(ctx, (launch, false), None, settings, &options) else {
        return Ok(None);
    };
    links::print_warnings(&started.warnings);
    let placed = start::Placed {
        space: Box::new((*started.workspace).clone().into()),
        operation: started.operation.clone(),
        entry: started.entry.clone(),
    };
    let code = start::run_attached(ctx, placed, (attach, settings)).await?;
    Ok(Some(code))
}

/// Report the workspace the daemon `launched`, whose agent, which `agent`
/// runs if it names one, runs in no terminal of this caller: once it is
/// recorded, or with `wait` once its preparation ends, unless the
/// preparation waits for other work or disk space, which leaves nothing to
/// follow yet.
pub(in crate::cli::commands) async fn report_launched(
    ctx: &Context,
    launched: Launched,
    (wait, agent): (bool, Option<crate::agent::Agent>),
) -> Result<i32> {
    let worker = matches!(launched.start, Started::Worker);
    let follows = wait
        && matches!(
            launched.start,
            Started::Preparing {
                operation: Some(_),
                ..
            }
        );
    let started = prepared::workspace_start(launched)?;
    match (worker, follows) {
        (true, _) => report_worker(ctx, started),
        (false, true) => follow_workspace(ctx, started, agent).await,
        (false, false) => report_workspace(ctx, started),
    }
}

/// Follow the preparation of the workspace the daemon `started`, and report
/// the workspace and the entry of its agent as the queue has it then.
async fn follow_workspace(
    ctx: &Context,
    started: StartedWorkspace,
    agent: Option<crate::agent::Agent>,
) -> Result<i32> {
    let entry = started.entry.clone();
    let preparing = crate::client::preparation::Preparing::follow(&ctx.paths, started).await?;
    let finished = finish_workspace(ctx, preparing, agent.as_ref(), false).await?;
    let Some(Prepared { workspace, .. }) = finished else {
        return Ok(1);
    };
    let Some(entry) = entry else {
        return Ok(0);
    };
    let entry = match queued_agents::current(ctx, &entry).await? {
        Some(entry) => entry,
        // It left the queue once its agent started.
        None => QueueEntry {
            state: QueueState::Starting,
            position: None,
            ..*entry
        },
    };
    start::report_queued_in(ctx, &workspace.name, &entry)
}

/// Report the existing workspace whose worker the daemon `started`, and
/// the entry of its agent, if it queued one.
fn report_worker(ctx: &Context, started: StartedWorkspace) -> Result<i32> {
    let StartedWorkspace {
        workspace, entry, ..
    } = started;
    ctx.show(&*workspace, |_| ())?;
    match entry {
        Some(entry) => start::report_queued_in(ctx, &workspace.name, &entry),
        None => Ok(0),
    }
}

/// Report the workspace the daemon `started`, whose preparation goes on
/// without this process, and the entry of its agent, which waits for it.
pub(super) fn report_workspace(ctx: &Context, started: StartedWorkspace) -> Result<i32> {
    links::print_warnings(&started.warnings);
    let StartedWorkspace {
        workspace,
        reused,
        entry,
        ..
    } = started;
    ctx.show(&*workspace, |workspace| {
        outln!(
            "{} {} {}; `shoal status {}` shows its preparation",
            if reused { "Opened" } else { "Created" },
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            crate::cli::commands::workspaces::location_phrase(workspace),
            workspace.number,
        );
    })?;
    match entry {
        Some(entry) => start::report_queued_in(ctx, &workspace.name, &entry),
        None => Ok(0),
    }
}
