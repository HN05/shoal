//! Stopping keeps agent sessions and commands for `shoal resume`; agents
//! stay down until it continues them. The other processes of a workspace,
//! and with `--processes` or `--leftovers` those of a chat, stop for good,
//! and the agent launches waiting in the queue leave it, having nothing to
//! save.
use anyhow::{Context as _, Result, ensure};
use futures_util::future::join_all;

use crate::client::output::agents::{
    CancelledRun, StoppedProcesses, StoppedWorkspaces, WorkspaceStopped,
};
use crate::{
    cli::{
        args::SpaceTargets,
        client::{self},
        commands::agent_queue,
        context::Context,
        output::Style,
        ui,
    },
    client::processes::{Selection, Stopped},
    execution::recovery,
    model::{SpaceKind, StoppedWorkspace, Workspace},
    protocol::{ErrorCode, QueuedLaunches, RemoteError},
};

pub(super) async fn run(
    ctx: &Context,
    targets: SpaceTargets,
    yes: bool,
    skip_children: bool,
) -> Result<i32> {
    if targets.all {
        return stop_all(ctx).await;
    }
    if let [selector] = targets.space.as_slice()
        && let Some(code) = agent_queue::remove_new(ctx, selector).await?
    {
        return Ok(code);
    }
    let mut selected = ui::select_workspaces(ctx, targets.space).await?;
    if !skip_children {
        selected = with_children(ctx, selected).await?;
    }
    if crate::env::inherits_scope(&ctx.paths.state) {
        ensure_below(ctx, &selected).await?;
    }
    match selected.as_slice() {
        [workspace] => stop_one(ctx, workspace.clone(), yes).await,
        workspaces => {
            let workspaces = inspect_each(ctx, workspaces).await?;
            stop_each(ctx, ui::Report::new("stop"), &workspaces, "", yes).await
        }
    }
}

async fn stop_one(ctx: &Context, workspace: String, yes: bool) -> Result<i32> {
    let stopped = stop(ctx, &workspace, yes, QueuedLaunches::Cancel).await?;
    if let Some(run) = &stopped.cancelled_run {
        // The job runs until its runner stops it, and its workspace goes then.
        ctx.emit_styled(
            Style::Success,
            &cancelled_run_message(&workspace, run),
            WorkspaceStopped {
                stopped: false,
                saved: false,
                other_processes: 0,
                cancelled: Vec::new(),
                cancelled_run: Some(run.clone()),
            },
        )?;
        return Ok(0);
    }
    let cancelled = stopped.cancelled;
    let other = stop_untracked(ctx, &workspace, Selection::Processes).await?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
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
        WorkspaceStopped {
            stopped: true,
            saved,
            other_processes: other.processes,
            cancelled,
            cancelled_run: None,
        },
    )?;
    Ok(0)
}

/// Stop the processes Shoal does not track that `selection` takes, of each
/// space `selectors` names, else of the current chat or workspace, else of
/// one picked. They are not saved for `shoal resume`.
pub(super) async fn processes(
    ctx: &Context,
    selectors: Vec<String>,
    selection: Selection,
) -> Result<i32> {
    // Unlike what ends a space, a stop of its processes starts from the chat
    // it runs in.
    let selectors = match ui::current_chat(&None) {
        Some(chat) if selectors.is_empty() => vec![chat],
        _ => ui::select_spaces(ctx, selectors, false).await?,
    };
    if selectors.len() > 1 {
        ensure_spaces(ctx, &selectors).await?;
    }
    let mut report = ui::Report::new("stop processes of");
    for selector in selectors {
        match stop_untracked(ctx, &selector, selection).await {
            Ok(stopped) => {
                let message = stopped_message(&stopped, selection);
                report.done(ctx, &message, stopped);
            }
            Err(error) => report.failed(ctx, &selector, &error),
        }
    }
    report.finish(ctx, "", |stopped, failed| StoppedProcesses {
        stopped,
        failed,
    })
}

/// Refuse `selectors` before any stops when one names neither a running
/// chat nor a workspace that stopping accepts.
async fn ensure_spaces(ctx: &Context, selectors: &[String]) -> Result<()> {
    let chats = client::chats(&ctx.paths).await?;
    let workspaces: Vec<String> = selectors
        .iter()
        .filter(|selector| {
            !chats
                .iter()
                .any(|chat| &&chat.id == selector || &&chat.name == selector)
        })
        .cloned()
        .collect();
    inspect_each(ctx, &workspaces).await?;
    Ok(())
}

/// Stop the untracked processes of `space` that `selection` takes, other
/// than this one and the processes that started it.
async fn stop_untracked(ctx: &Context, space: &str, selection: Selection) -> Result<Stopped> {
    let requester = Some(std::process::id());
    crate::client::stopping::stop_processes(&ctx.paths, space.to_owned(), selection, requester)
        .await
}

/// What a stop of `selection` reports for `stopped`.
fn stopped_message(stopped: &Stopped, selection: Selection) -> String {
    let (one, many) = match selection {
        Selection::Processes => ("untracked process", "untracked processes"),
        Selection::Leftovers => ("leftover", "leftovers"),
    };
    match stopped.processes {
        0 => format!("No {many} in {}", stopped.space),
        1 => format!("Stopped 1 {one} in {}", stopped.space),
        n => format!("Stopped {n} {many} in {}", stopped.space),
    }
}

/// What `shoal stop` says once the forge was asked to cancel `run`, the run
/// of the CI job in `workspace`.
fn cancelled_run_message(workspace: &str, run: &str) -> String {
    format!("Asked the forge to cancel {run}; {workspace} goes once its CI job's runner stops it")
}

/// How `shoal stop` names the `count` agent launches it removed from the
/// agent queue.
fn removed_launches(count: usize) -> String {
    match count {
        1 => "Removed 1 queued agent from the agent queue".to_owned(),
        n => format!("Removed {n} queued agents from the agent queue"),
    }
}

/// How plain `shoal stop` names the untracked processes it stopped besides
/// the saved agents and commands.
fn other_processes(count: usize) -> String {
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
    workspace: &str,
    yes: bool,
    launches: QueuedLaunches,
) -> Result<StoppedWorkspace> {
    let request_stop = |acknowledge_unproven| {
        crate::client::stopping::stop_workspace(
            &ctx.paths,
            workspace.to_owned(),
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

/// The IDs of the workspaces `selectors` name, each followed by the
/// workspaces below it, through spaces of either kind, which a stop of it
/// stops too: their agents work for it.
async fn with_children(ctx: &Context, selectors: Vec<String>) -> Result<Vec<String>> {
    let spaces = client::spaces(&ctx.paths).await?;
    let mut found: Vec<String> = Vec::new();
    for selector in selectors {
        let id = client::inspect(&ctx.paths, selector).await?.workspace.id;
        let below = spaces.below(&id).into_iter();
        let workspaces = below.filter(|space| space.kind == SpaceKind::Workspace);
        for id in std::iter::once(id).chain(workspaces.map(|space| space.id)) {
            if !found.contains(&id) {
                found.push(id);
            }
        }
    }
    Ok(found)
}

/// Refuse a stop in a scoped process, a workspace's or a chat's agent, that
/// `selected` includes its own workspace in. The daemon refuses each
/// workspace not below the caller's space, which needs the scope token;
/// refusing its own here, before any stops, keeps the stop from taking the
/// workspaces below it alone.
pub(super) async fn ensure_below(ctx: &Context, selected: &[String]) -> Result<()> {
    const REFUSAL: &str =
        "scoped processes stop only the workspaces below theirs; finish their own with shoal done";
    let own = std::env::var(crate::env::WORKSPACE_ID)
        .ok()
        .filter(|own| !own.is_empty())
        .or_else(crate::env::in_chat)
        .context(REFUSAL)?;
    ensure!(crate::env::is_scoped(), "{REFUSAL}");
    for selector in selected {
        let id = client::inspect(&ctx.paths, selector.clone())
            .await?
            .workspace
            .id;
        ensure!(id != own, "{REFUSAL}");
    }
    Ok(())
}

/// The workspaces `selectors` name, refusing them all before any stops when
/// one is unknown or stopping it would be refused now, as while another
/// lifecycle operation or a resource hook runs.
async fn inspect_each(ctx: &Context, selectors: &[String]) -> Result<Vec<Workspace>> {
    let mut workspaces = Vec::new();
    let mut refusals = Vec::new();
    for selector in selectors {
        let workspace = match client::inspect(&ctx.paths, selector.clone()).await {
            Ok(inspection) => inspection.workspace,
            Err(error) => {
                refusals.push(format!("{selector}: {error:#}"));
                continue;
            }
        };
        match crate::client::stopping::check(&ctx.paths, workspace.id.clone()).await {
            Ok(()) => workspaces.push(workspace),
            Err(error) => refusals.push(format!("{}: {error:#}", workspace.name)),
        }
    }
    ensure!(
        refusals.is_empty(),
        "cannot stop these workspaces, so none was stopped:\n  {}",
        refusals.join("\n  ")
    );
    Ok(workspaces)
}

/// Stop every settled workspace with a running execution, an agent whose
/// restore is queued or requested, or an agent launch waiting to start in
/// it, which stopping cancels; the daemon selects them while no restore or
/// launch can move between the queue and its agent.
async fn stop_all(ctx: &Context) -> Result<i32> {
    let report = ui::Report::new("stop");
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
    stop_each(ctx, report, &running, "--all", false).await
}

/// Stop `workspaces` in parallel, adding to `report`; one failure does not
/// keep the others running, and an execution Shoal cannot prove ended fails
/// its workspace unless `acknowledge_unproven`. `resume` is what the hint
/// to continue them passes `shoal resume`; empty, it names each one.
async fn stop_each(
    ctx: &Context,
    mut report: ui::Report<Outcome>,
    workspaces: &[Workspace],
    resume: &str,
    acknowledge_unproven: bool,
) -> Result<i32> {
    let results = join_all(workspaces.iter().map(|workspace| async {
        let stopped = crate::client::stopping::stop_workspace(
            &ctx.paths,
            workspace.id.clone(),
            acknowledge_unproven,
            QueuedLaunches::Cancel,
        )
        .await?;
        let other = match stopped.cancelled_run {
            Some(_) => None,
            None => Some(stop_untracked(ctx, &workspace.id, Selection::Processes).await?),
        };
        anyhow::Ok((other, stopped))
    }))
    .await;
    let mut resumable = Vec::new();
    let mut removed = Vec::new();
    for (workspace, result) in workspaces.iter().zip(results) {
        let name = workspace.name.as_str();
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
                    workspace: name.to_owned(),
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
                report.done(ctx, &message, Outcome::Stopped(name.to_owned()));
                if has_saved_work(ctx, workspace) {
                    resumable.push(name);
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
    Stopped(String),
    /// A CI job's workspace, whose run the forge was asked to cancel.
    Cancelled(CancelledRun),
}

/// Whether `workspace` saved work for `shoal resume`, which refuses to
/// resume a workspace without any.
fn has_saved_work(ctx: &Context, workspace: &Workspace) -> bool {
    recovery::Saved::load(&ctx.paths, &workspace.id).is_ok_and(|saved| !saved.is_empty())
}

/// How to continue what `shoal stop` stopped in `targets`, the arguments of
/// `shoal resume`, and how to take agents down only briefly instead.
fn continue_hint(targets: &str) -> String {
    format!(
        "Continue with shoal resume {targets}. To restart agents without stopping them, use shoal restart"
    )
}
