//! What a start reports of the workspace a launch recorded, and how a
//! creation without an agent enters it.
use anyhow::Result;

use super::prepared;
use crate::{
    cli::{
        commands::{
            links, queued_agents,
            start::{self, StartOptions},
        },
        context::Context,
        output::{Palette, Style},
    },
    client::preparation::{Preparing, Unprepared},
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
use shoal_workers::starts::{Starting, finish_workspace};

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
    let Some(attach) = start::attach(ctx, (launch, false), None, settings, &options) else {
        return Ok(None);
    };
    links::print_warnings(&started.warnings);
    let placed = shoal_workers::starts::Placed {
        space: Box::new((*started.workspace).clone().into()),
        operation: started.operation.clone(),
        entry: started.entry.clone(),
    };
    let code = shoal_workers::starts::run(ctx, placed, (attach, settings)).await?;
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
    let machine = launched.machine.clone();
    let started = prepared::workspace_start(launched)?;
    match (worker, follows, machine) {
        (true, ..) => report_worker(ctx, started),
        (false, true, Some(machine)) => follow_elsewhere(ctx, started, &machine).await,
        (false, true, None) => follow_workspace(ctx, started, agent).await,
        (false, false, _) => report_workspace(ctx, started),
    }
}

/// Follow the preparation of the workspace `machine`, another joined
/// machine, `started`, of which nothing runs in this caller, and report
/// the workspace and the entry of its agent, which that machine's queue
/// starts, once it ends.
async fn follow_elsewhere(ctx: &Context, started: StartedWorkspace, machine: &str) -> Result<i32> {
    let reused = started.reused;
    let Prepared { workspace, agent } = prepared_on(ctx, started, Some(machine)).await?;
    ctx.prepared(&workspace, reused)?;
    match agent {
        Some(entry) => start::report_queued_in(ctx, &workspace.name, &entry),
        None => Ok(0),
    }
}

/// The workspace `started` once its preparation, which `machine` runs
/// where another joined machine than this one does, ended, without
/// entering it or offering a retry, which act on its worktree; a failure
/// names what shows and retries it.
pub(in crate::cli::commands) async fn prepared_on(
    ctx: &Context,
    started: StartedWorkspace,
    machine: Option<&str>,
) -> Result<Prepared> {
    let preparing = Preparing::follow(&ctx.paths, started).await?;
    match shoal_workers::preparation::follow(&ctx.paths, ctx, preparing).await? {
        Ok(prepared) => Ok(prepared),
        Err(Unprepared { workspace, failure }) => {
            let number = workspace.number;
            let there = machine.map(|machine| format!(" on machine {machine}"));
            Err(crate::operations::error(failure).context(format!(
                "preparing {}{} failed; `shoal status {number}` shows it, and `shoal setup {number}` on its machine retries it",
                workspace.name,
                there.unwrap_or_default()
            )))
        }
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
            location_phrase(workspace),
            workspace.number,
        );
    })?;
    match entry {
        Some(entry) => start::report_queued_in(ctx, &workspace.name, &entry),
        None => Ok(0),
    }
}

/// Where the workspace's worktree is, for messages: `on branch <branch> at
/// <path>`, or `at <commit> in <path>` with what a workspace at a commit was
/// made for.
pub(in crate::cli::commands) fn location_phrase(workspace: &crate::model::Workspace) -> String {
    use crate::model::Head;
    let path = workspace.path.display();
    match &workspace.head {
        Head::Branch(branch) => format!("on branch {branch} at {path}"),
        Head::Commit(at) => format!("at {} in {path}", at.label()),
    }
}
