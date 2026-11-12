//! `shoal resume`: choosing how the selected spaces resume, which the
//! modules below carry out. Restores use saved agent identity and today's
//! restore configuration; they queue unless asked to start now, the stopped
//! chats restore at once, a workspace whose setup failed retries it before
//! the agent waiting for it starts, and `--discard` forgets the stopped work
//! instead.
use crate::client::execution_ids::ExecutionId;
use crate::client::output::resume::{Queued, Resumed};
use crate::client::saved::Queuing;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{args::SpaceTargets, commands::turns::Continuation, context::Context},
    config::permissions::PermissionChange,
};
use anyhow::{Result, ensure};
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
/// chat in this terminal, and so do the agents waiting for the failed
/// setups of named workspaces, which retry them first. Without a terminal
/// every restore queues, which `now` starts at once, since this process can
/// start none of them. Every target, and a change to the permissions given
/// for every selected agent and chat, is checked before any setup or
/// restore.
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
    if named && !discard && execution.is_none() {
        setups::take_failed(ctx, &mut selected).await?;
    }
    if named
        && !discard
        && execution.is_none()
        && let Some(code) = continue_turns(ctx, &mut selected, change.is_some()).await?
    {
        return Ok(code);
    }
    selected.refuse_running()?;
    if let Some(change) = change {
        permission_change::check(ctx, &selected, execution.as_ref(), change).await?;
    }
    if scoped && !discard {
        return queue::queue_below(ctx, selected, execution, change).await;
    }
    let queuing = Queuing::of(ctx.on_terminal(), now);
    let setups = selected.setups.as_slice();
    match (
        selected.chats.as_slice(),
        selected.workspaces.as_deref(),
        setups,
    ) {
        ([], Some([workspace]), []) if discard => {
            discard::discard_work(ctx, workspace.clone(), execution).await
        }
        ([], Some([workspace]), []) if focus || queuing == Queuing::Here { now: true } => {
            workspace::run(ctx, workspace.clone(), execution, focus, change).await
        }
        ([], Some([workspace]), []) => {
            queue::queue_workspace(ctx, workspace.clone(), execution, queuing, change).await
        }
        ([chat], Some([]), []) if focus && execution.is_none() => {
            permission_change::apply(ctx, &[], std::slice::from_ref(chat), &[], change).await?;
            chats::restore_here(ctx, chat, change).await
        }
        ([], Some([]), [setup]) if focus => setups::retry_here(ctx, setup, change).await,
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
/// workspaces and running chats, which keeps the workspaces without one,
/// refusing those, the running chats without one and the selected stopped
/// chats, which a request that also resumes stopped work would report apart
/// from it, and a `changing` --permission, which a running agent cannot
/// take. `None` when no turn ended so.
async fn continue_turns(
    ctx: &Context,
    selected: &mut Selected,
    changing: bool,
) -> Result<Option<i32>> {
    let Some(workspaces) = selected.workspaces.take() else {
        return Ok(None);
    };
    let (mut continuation, rest) = Continuation::plan(ctx, workspaces, Clone::clone).await?;
    let workspaces_left = !rest.is_empty();
    selected.workspaces = Some(rest);
    let running = std::mem::take(&mut selected.running);
    let (chats, idle) =
        Continuation::plan(ctx, running, |chat| SpaceSelector::id(&chat.id)).await?;
    selected.running = idle;
    continuation.extend(chats);
    if continuation.is_empty() {
        return Ok(None);
    }
    selected.refuse_running()?;
    ensure!(
        !workspaces_left && selected.chats.is_empty() && selected.setups.is_empty(),
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
/// `discard`, retry the failed setups, and queue the restores of the
/// workspaces' stopped agents and the agents waiting for those setups,
/// start them as `queuing` says, or forget the stopped work for `discard`.
async fn resume_each(
    ctx: &Context,
    selected: Selected,
    discard: bool,
    queuing: Queuing,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let stopped = selected.stopped(ctx, discard).await?;
    if stopped.is_empty() && selected.chats.is_empty() && selected.setups.is_empty() {
        reports::nothing(ctx, discard || queuing == Queuing::Here { now: true })?;
        return Ok(0);
    }
    if discard {
        return discard::discard_each(ctx, &stopped, &selected.chats).await;
    }
    if let Queuing::Daemon { .. } = queuing {
        queue::ensure_daemon_starts(ctx, &stopped).await?;
    }
    permission_change::apply(ctx, &stopped, &selected.chats, &selected.setups, change).await?;
    let now = matches!(queuing, Queuing::Here { now } | Queuing::Daemon { now } if now);
    let resumed = set_up_and_restore_chats(ctx, &selected, change, now).await;
    match queuing {
        Queuing::Here { now: true } => {
            let resumed = Resumed {
                chats: resumed.chats,
                set_up: resumed.set_up,
                failed: resumed.failed,
                ..Default::default()
            };
            sessions::run_all(ctx, stopped, resumed).await
        }
        Queuing::Here { now } | Queuing::Daemon { now } => {
            queue::queue_all(ctx, stopped, resumed, now).await
        }
    }
}

/// Retry the `selected` failed setups, starting the agents waiting for them
/// at once when `now`, and restore the `selected` stopped chats in a
/// background place under `change`, going on after one fails, and report
/// them.
async fn set_up_and_restore_chats(
    ctx: &Context,
    selected: &Selected,
    change: Option<&PermissionChange>,
    now: bool,
) -> Queued {
    let (set_up, mut failed) = setups::retry_each(ctx, &selected.setups, now).await;
    let (chats, chats_failed) = chats::restore_each(ctx, &selected.chats, change).await;
    failed.extend(chats_failed);
    Queued {
        chats,
        set_up,
        failed,
        ..Default::default()
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
mod setups;
mod stale;
mod targets;
mod workspace;
