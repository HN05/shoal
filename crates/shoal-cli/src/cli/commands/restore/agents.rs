//! Restoring the stopped agents of the selected spaces, which the modules
//! below carry out. Restores use saved agent identity and today's restore
//! configuration; they queue unless asked to start now, a chat's starting
//! at once, a workspace whose setup failed retries it before the agent
//! waiting for it starts, and `--forget` forgets the stopped work instead.
use crate::client::execution_ids::ExecutionId;
use crate::client::output::restore::{Queued, Reopened, StartedRestores};
use crate::client::saved::Queuing;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{args::SpaceTargets, commands::turns::Continuation, context::Context},
    config::permissions::PermissionChange,
    model::PresentSpace,
};
use anyhow::{Context as _, Result, ensure};

use super::targets::Selected;

/// How `shoal restore` restores the agents of the spaces it selects.
pub struct Options {
    /// The one stopped execution of a space to restore.
    pub execution: Option<ExecutionId>,
    /// Forget the stopped work instead of restoring it.
    pub forget: bool,
    /// Start the restores at once, regardless of the agent limits.
    pub now: bool,
    /// Restore one agent in this terminal.
    pub focus: bool,
    /// What changes in the permissions the restored agents were given;
    /// without it they keep them.
    pub change: Option<PermissionChange>,
}

/// Restore the `selected` spaces, every one with stopped work for `--all`,
/// and open the removed workspaces among them again once every target is
/// checked; `named` says the command line named them. Restores queue
/// unless `now`, or `focus`, which restores one agent in this terminal, and
/// so do the agents waiting for the failed setups of named workspaces,
/// which retry them first; a chat's start at once from the queue, which
/// admits them without a slot. Without a terminal every restore queues,
/// which `now` starts at once, since this process can start none of them.
/// Every target, and a change to the permissions given for every selected
/// agent, is checked before any setup or restore.
pub(super) async fn restore(
    ctx: &Context,
    mut selected: Selected,
    named: bool,
    options: Options,
) -> Result<i32> {
    let Options {
        execution,
        forget,
        now,
        focus,
        change,
    } = options;
    let change = change.as_ref();
    let scoped = crate::env::inherits_scope(&ctx.paths.state);
    ensure!(
        !scoped || selected.removed.is_empty(),
        "workspace processes cannot reopen removed workspaces; ask the user to run shoal restore"
    );
    if named && !forget && execution.is_none() {
        setups::take_failed(ctx, &mut selected).await?;
    }
    if named
        && !forget
        && execution.is_none()
        && let Some(code) = continue_turns(ctx, &mut selected, change.is_some()).await?
    {
        return Ok(code);
    }
    selected.refuse_running()?;
    if let Some(change) = change {
        permission_change::check(ctx, &selected, execution.as_ref(), change).await?;
    }
    if scoped && !forget {
        return queue::queue_below(ctx, selected, execution, change).await;
    }
    let Some(reopened) = super::reopen::reopen_each(ctx, &selected.removed).await? else {
        return Ok(1);
    };
    let queuing = Queuing::of(ctx.on_terminal(), now);
    let setups = selected.setups.as_slice();
    let alone = reopened.is_empty();
    match (selected.spaces.as_deref(), setups) {
        (Some([space]), []) if forget && alone => {
            forget::forget_work(ctx, space.clone(), execution).await
        }
        (Some([space]), []) if focus => {
            reports::reopened(ctx, reopened)?;
            space::run(ctx, space.clone(), execution, true, change).await
        }
        (Some([space]), []) if alone && queuing == Queuing::Here { now: true } => {
            space::run(ctx, space.clone(), execution, false, change).await
        }
        (Some([space]), []) if alone => {
            queue::queue_space(ctx, space.clone(), execution, queuing, change).await
        }
        (Some([]), [setup]) if focus => {
            reports::reopened(ctx, reopened)?;
            setups::retry_here(ctx, setup, change).await
        }
        _ => {
            ensure!(
                execution.is_none(),
                "--execution selects a stopped execution of one space"
            );
            ensure!(!focus, "--focus restores the agent of one space");
            restore_each(ctx, selected, reopened, forget, queuing, change).await
        }
    }
}

/// Continue the turns an interrupt ended of the agents in the `selected`
/// spaces and running chats, which keeps the spaces without one, refusing
/// those and the running chats without one, which a request that also
/// resumes stopped work would report apart from it, and a `changing`
/// --permission, which a running agent cannot take. `None` when no turn
/// ended so.
async fn continue_turns(
    ctx: &Context,
    selected: &mut Selected,
    changing: bool,
) -> Result<Option<i32>> {
    let Some(spaces) = selected.spaces.take() else {
        return Ok(None);
    };
    let (mut continuation, rest) = Continuation::plan(ctx, spaces, Clone::clone).await?;
    let spaces_left = !rest.is_empty();
    selected.spaces = Some(rest);
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
        !spaces_left && selected.setups.is_empty() && selected.removed.is_empty(),
        "restore interrupted turns and stopped work separately"
    );
    ensure!(
        !changing,
        "an interrupted turn continues in its running agent, which keeps its permissions; restart the agent with --permission instead"
    );
    continuation.submit(ctx).await.map(Some)
}

/// Refuse a restore in a scoped process, a workspace's or a chat's agent,
/// that does not name the spaces below its own or restores in this terminal,
/// `here`: it only queues the restores of the agents it stopped there, which
/// the daemon starts, or forgets their stopped work.
pub(super) fn ensure_below(targets: &SpaceTargets, here: bool) -> Result<()> {
    // The daemon checks that each space is below the caller's, which needs
    // the scope token: a process that cleared it stays refused.
    ensure!(
        crate::env::is_scoped() && !targets.all && !targets.space.is_empty() && !here,
        "scoped processes restore only the spaces below theirs, by number and without --now or --focus: shoal restore <space> [--forget]"
    );
    Ok(())
}

/// Restore the `selected` spaces together, reporting them at once with the
/// workspaces `reopened` for them: retry the failed setups, and queue the
/// restores of the stopped agents and the agents waiting for those setups,
/// making `change` when given, start them as `queuing` says, or forget the
/// stopped work for `forget`.
async fn restore_each(
    ctx: &Context,
    selected: Selected,
    reopened: Vec<Reopened>,
    forget: bool,
    queuing: Queuing,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let stopped = saved::stopped(ctx, &selected, forget).await?;
    if stopped.is_empty() && selected.setups.is_empty() {
        let started = forget || queuing == Queuing::Here { now: true };
        reports::nothing(ctx, reopened, started)?;
        return Ok(0);
    }
    if forget {
        return forget::forget_each(ctx, &stopped, reopened).await;
    }
    if let Queuing::Daemon { .. } = queuing {
        queue::ensure_daemon_starts(ctx, &stopped).await?;
    }
    permission_change::apply(ctx, &stopped, &selected.setups, change).await?;
    let now = matches!(queuing, Queuing::Here { now } | Queuing::Daemon { now } if now);
    let restored = set_up(ctx, &selected, reopened, now).await;
    match queuing {
        Queuing::Here { now: true } => {
            let restored = StartedRestores {
                set_up: restored.set_up,
                reopened: restored.reopened,
                failed: restored.failed,
                ..Default::default()
            };
            sessions::run_all(ctx, stopped, restored).await
        }
        Queuing::Here { now } | Queuing::Daemon { now } => {
            queue::queue_all(ctx, stopped, restored, now).await
        }
    }
}

/// Retry the `selected` failed setups, starting the agents waiting for them
/// at once when `now`, going on after one fails, and report them with the
/// workspaces `reopened` for them.
async fn set_up(ctx: &Context, selected: &Selected, reopened: Vec<Reopened>, now: bool) -> Queued {
    let (set_up, failed) = setups::retry_each(ctx, &selected.setups, now).await;
    Queued {
        set_up,
        reopened,
        failed,
        ..Default::default()
    }
}

/// The space `selector` names, a workspace or a chat.
async fn present_space(ctx: &Context, selector: &SpaceSelector) -> Result<PresentSpace> {
    let space = crate::client::workspaces::present_space(&ctx.paths, selector).await?;
    space.with_context(|| format!("no space {selector}"))
}

mod commands;
mod forget;
pub(in crate::cli::commands) mod permission_change;
mod picker;
mod places;
pub(in crate::cli::commands) mod queue;
pub(in crate::cli::commands) mod queued;
pub(in crate::cli::commands) mod reports;
mod saved;
mod sessions;
pub(super) mod setups;
mod space;
mod stale;
