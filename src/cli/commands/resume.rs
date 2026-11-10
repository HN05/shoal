//! `shoal resume`: choosing how the selected spaces resume, which the
//! modules below carry out. Restores use saved agent identity and today's
//! restore configuration; they queue unless asked to start now, the stopped
//! chats restore at once, and `--discard` forgets the stopped work instead.
use crate::client::execution_ids::ExecutionId;
use crate::client::output::resume::Resumed;
use crate::{
    cli::{args::SpaceTargets, commands::turns::Continuation, context::Context},
    config::permissions::PermissionChange,
};
use anyhow::{Result, ensure};
use queue::Queuing;
use targets::Selected;

/// How `shoal resume` restores the spaces it selects.
pub struct Options {
    /// The one stopped execution of a workspace to restore.
    pub execution: Option<ExecutionId>,
    /// Forget the stopped work instead of restoring it.
    pub discard: bool,
    /// Start the restores at once, regardless of the agent limits.
    pub now: bool,
    /// Restore one agent or chat in this terminal.
    pub focus: bool,
    /// What changes in the permissions the restored agents and chats were
    /// given; without it they keep them.
    pub change: Option<PermissionChange>,
}

/// `shoal resume`: the selected spaces, or every one for `--all` or when no
/// space has anything to resume. Stopped chats restore at once; workspaces'
/// restores queue unless `now`, or `focus`, which restores one agent or
/// chat in this terminal. Without a terminal every restore queues, which
/// `now` starts at once, since this process can start none of them. A
/// change to the permissions given is checked for every selected agent and
/// chat before any restores.
pub async fn select(ctx: &Context, targets: SpaceTargets, options: Options) -> Result<i32> {
    let Options {
        execution,
        discard,
        now,
        focus,
        change,
    } = options;
    let change = change.as_ref();
    let scoped = crate::env::inherits_scope(&ctx.paths.state);
    if scoped {
        ensure_below(&targets, now || focus)?;
    }
    let named = !targets.space.is_empty();
    let mut selected = Selected::load(ctx, targets).await?;
    if named
        && !discard
        && execution.is_none()
        && let Some(code) = continue_turns(ctx, &mut selected, change.is_some()).await?
    {
        return Ok(code);
    }
    if let Some(change) = change {
        permission_change::check(ctx, &selected, execution.as_ref(), change).await?;
    }
    if scoped && !discard {
        return queue::queue_below(ctx, selected, execution, change).await;
    }
    let queuing = Queuing::of(ctx, now);
    match (selected.chats.as_slice(), selected.workspaces.as_deref()) {
        ([], Some([workspace])) if discard => {
            discard::discard_work(ctx, workspace.clone(), execution).await
        }
        ([], Some([workspace])) if focus || queuing == Queuing::Here { now: true } => {
            workspace::run(ctx, workspace.clone(), execution, focus, change).await
        }
        ([], Some([workspace])) => {
            queue::queue_workspace(ctx, workspace.clone(), execution, queuing, change).await
        }
        ([chat], Some([])) if focus && execution.is_none() => {
            permission_change::apply(ctx, &[], std::slice::from_ref(chat), change).await?;
            chats::restore_here(ctx, chat, change).await
        }
        _ => {
            ensure!(
                execution.is_none(),
                "--execution selects a stopped execution of one workspace"
            );
            ensure!(
                !focus,
                "--focus restores the agent of one workspace or chat"
            );
            resume_each(ctx, selected, discard, queuing, change).await
        }
    }
}

/// Continue the turns an interrupt ended of the agents in the `selected`
/// workspaces, which keeps the workspaces without one, refusing those and
/// selected chats, which a request that also resumes stopped work would
/// report apart from it, and a `changing` --permission, which a running
/// agent cannot take. `None` when no turn ended so.
async fn continue_turns(
    ctx: &Context,
    selected: &mut Selected,
    changing: bool,
) -> Result<Option<i32>> {
    let Some(workspaces) = &mut selected.workspaces else {
        return Ok(None);
    };
    let (continuation, rest) = Continuation::plan(ctx, std::mem::take(workspaces)).await?;
    *workspaces = rest;
    if continuation.is_empty() {
        return Ok(None);
    }
    ensure!(
        workspaces.is_empty() && selected.chats.is_empty(),
        "resume interrupted turns and stopped work separately"
    );
    ensure!(
        !changing,
        "an interrupted turn continues in its running agent, which keeps its permissions; restart the agent with --permission instead"
    );
    continuation.submit(ctx).await.map(Some)
}

/// Refuse a resume in a scoped process, a workspace's or a chat's agent,
/// that does not name the spaces below its own or restores in this terminal,
/// `here`: it only queues the restores of the agents it stopped there, which
/// the daemon starts, and restores the chats it stopped in a background
/// place, or discards their stopped work.
fn ensure_below(targets: &SpaceTargets, here: bool) -> Result<()> {
    // The daemon checks that each space is below the caller's, which needs
    // the scope token: a process that cleared it stays refused.
    ensure!(
        crate::env::is_scoped() && !targets.all && !targets.space.is_empty() && !here,
        "scoped processes resume only the spaces below theirs, by name and without --now or --focus: shoal resume <space> [--discard]"
    );
    Ok(())
}

/// Resume the `selected` spaces together, reporting them at once: restore
/// the stopped chats, making `change` when given, or remove them for
/// `discard`, and queue the restores of the workspaces' stopped agents,
/// start them as `queuing` says, or forget the stopped work for `discard`.
async fn resume_each(
    ctx: &Context,
    selected: Selected,
    discard: bool,
    queuing: Queuing,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let stopped = selected.stopped(ctx, discard).await?;
    if stopped.is_empty() && selected.chats.is_empty() {
        reports::nothing(ctx, discard || queuing == Queuing::Here { now: true })?;
        return Ok(0);
    }
    if discard {
        return discard::discard_each(ctx, &stopped, &selected.chats).await;
    }
    if let Queuing::Daemon { .. } = queuing {
        queue::ensure_daemon_starts(ctx, &stopped).await?;
    }
    permission_change::apply(ctx, &stopped, &selected.chats, change).await?;
    let (chats, failed) = chats::restore_each(ctx, &selected.chats, change).await;
    match queuing {
        Queuing::Here { now: true } => {
            let resumed = Resumed {
                chats,
                failed,
                ..Default::default()
            };
            sessions::run_all(ctx, stopped, resumed).await
        }
        Queuing::Here { now: false } => queue::queue_all(ctx, stopped, chats, failed, false).await,
        Queuing::Daemon { now } => queue::queue_all(ctx, stopped, chats, failed, now).await,
    }
}

mod chats;
mod commands;
mod discard;
pub(super) mod permission_change;
mod picker;
pub(super) mod queue;
pub(super) mod queued;
pub(super) mod reports;
mod sessions;
mod stale;
mod targets;
mod workspace;

#[cfg(test)]
mod tests;
