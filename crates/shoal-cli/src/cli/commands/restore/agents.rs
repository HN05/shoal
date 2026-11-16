//! Restoring the stopped agents of the selected spaces, which the modules
//! below carry out. Restores use saved agent identity and today's restore
//! configuration; they queue unless asked to start now, a chat's starting
//! at once, a workspace whose setup failed retries it before the agent
//! waiting for it starts, and `--forget` forgets the stopped work instead.
use crate::client::output::{
    batch::Failure,
    restore::{Queued, Reopened, StartedRestores},
};
use crate::{
    cli::{commands::turns::Continuation, context::Context, ui},
    config::permissions::PermissionChange,
    execution::recovery::Saved,
    model::Space,
};
use anyhow::{Context as _, Result, ensure};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::saved::Queuing;
use shoal_protocol::space_names::SpaceSelector;

use super::targets::Selected;

/// How `shoal restore` restores the agents of the spaces it selects.
pub struct Options {
    /// The one stopped execution of a space to restore.
    pub execution: Option<ExecutionId>,
    /// Forget the stopped work instead of restoring it.
    pub forget: bool,
    /// Restore everything but the agents, which stay saved.
    pub no_agents: bool,
    /// Start the restores at once, regardless of the agent limits.
    pub now: bool,
    /// Restore one agent in this terminal.
    pub focus: bool,
    /// What changes in the permissions the restored agents were given;
    /// without it they keep them.
    pub change: Option<PermissionChange>,
}

/// Restore the `selected` spaces, every one with stopped work for `--all`,
/// opening the removed workspaces among them again once every target is
/// checked, whose saved agents then restore with the others, and
/// continuing the turns an interrupt ended in the named ones as the rest
/// restores; `named` says the command line named them. Restores queue
/// unless `now`, or `focus`, which restores one agent in this terminal, and
/// so do the agents waiting for the failed setups of named workspaces,
/// which retry them first; a chat's start at once from the queue, which
/// admits them without a slot. Without a terminal every restore queues,
/// which `now` starts at once, since this process can start none of them.
/// Every target, and a change to the permissions given for every selected
/// agent, is checked before any setup, reopening, continued turn or
/// restore.
pub(super) async fn restore(
    ctx: &Context,
    mut selected: Selected,
    named: bool,
    options: Options,
) -> Result<i32> {
    let scoped = crate::env::inherits_scope(&ctx.paths.state);
    let change = options.change.as_ref();
    let restores_agents = !options.forget && !options.no_agents;
    let whole = named && !options.forget && options.execution.is_none();
    if whole {
        setups::take_failed(ctx, &mut selected).await?;
    }
    let turns = match whole && !options.no_agents {
        true => interrupted_turns(ctx, &mut selected, change.is_some()).await?,
        false => Continuation::default(),
    };
    selected.refuse_running()?;
    if !turns.is_empty() && selected.is_empty() {
        return turns.submit(ctx).await;
    }
    if let Some(change) = change {
        permission_change::check(ctx, &selected, change).await?;
    }
    if scoped && restores_agents {
        let mut reopened = super::reopen::reopen_each(ctx, &selected.removed).await?;
        select_reopened(ctx, &mut selected, &mut reopened).await?;
        let now = options.now || options.focus;
        let alongside = Alongside::turns(turns);
        return queue::queue_below(ctx, selected, options.execution, change, now, alongside).await;
    }
    ensure_one_agent(&selected, &options)?;
    let reopened = super::reopen::reopen_each(ctx, &selected.removed).await?;
    let reopening = super::reopen::Reopening::of(&reopened);
    let alongside = Alongside { reopened, turns };
    let restored = restore_reopened(ctx, selected, alongside, options).await;
    restored.map_err(|error| reopening.explain(error))
}

/// What a restore does beside restoring stopped agents, which it reports
/// with them: the workspaces it reopened, and the turns an interrupt ended,
/// which it continues once nothing is left to refuse the restore.
pub(super) struct Alongside {
    reopened: Vec<Reopened>,
    turns: Continuation,
}

impl Alongside {
    /// Only the interrupted `turns`, for a restore that reports no reopened
    /// workspace.
    fn turns(turns: Continuation) -> Self {
        Self {
            reopened: Vec::new(),
            turns,
        }
    }

    /// Whether there is nothing to report beside the agents restored.
    fn is_empty(&self) -> bool {
        self.reopened.is_empty() && self.turns.is_empty()
    }

    /// Continue the interrupted turns, returning what to report: the
    /// reopened workspaces, the continued turns and the failures to
    /// continue them.
    async fn continue_turns(self, ctx: &Context) -> Queued {
        let continued = self.turns.submit_with_others(ctx).await;
        Queued {
            reopened: self.reopened,
            continued: continued.continued,
            failed: continued.failed,
            ..Default::default()
        }
    }
}

/// Restore the `selected` spaces once the workspaces reopened for them are
/// open again, with what it does `alongside`: everything but the agents for
/// `--no-agents`, otherwise their agents too, the reopened workspaces'
/// among them.
async fn restore_reopened(
    ctx: &Context,
    mut selected: Selected,
    mut alongside: Alongside,
    options: Options,
) -> Result<i32> {
    if options.no_agents {
        return restore_without_agents(ctx, &selected, alongside).await;
    }
    select_reopened(ctx, &mut selected, &mut alongside.reopened).await?;
    restore_selected(ctx, selected, alongside, options).await
}

/// Refuse `--execution`, which selects an agent of one space, and
/// `--focus`, which restores one agent here, unless the `selected` spaces
/// come to one, counting each removed workspace whose removal saved agents,
/// or for `--focus` to one failed setup, before anything opens again.
fn ensure_one_agent(selected: &Selected, options: &Options) -> Result<()> {
    let Some(spaces) = &selected.spaces else {
        return Ok(());
    };
    let reopened = (selected.removed.iter())
        .filter(|workspace| !(workspace.agents.is_empty() && workspace.last_sessions.is_empty()))
        .count();
    let spaces = spaces.len() + reopened;
    ensure!(
        options.execution.is_none() || spaces == 1,
        "--execution selects a stopped execution of one space"
    );
    ensure!(
        !options.focus || spaces + selected.setups.len() == 1,
        "--focus restores the agent of one space"
    );
    Ok(())
}

/// Restore the agents of the `selected` spaces as `options` says,
/// with what it does `alongside`: one space's alone or in this terminal,
/// the agent waiting for one failed setup in this terminal, or every one
/// together.
async fn restore_selected(
    ctx: &Context,
    selected: Selected,
    alongside: Alongside,
    options: Options,
) -> Result<i32> {
    let Options {
        execution,
        forget,
        now,
        focus,
        change,
        ..
    } = options;
    let change = change.as_ref();
    let queuing = Queuing::of(ctx.on_terminal(), now);
    let setups = selected.setups.as_slice();
    let alone = alongside.is_empty();
    match (selected.spaces.as_deref(), setups) {
        (Some([space]), []) if forget && alone => {
            forget::forget_work(ctx, space.clone(), execution).await
        }
        (Some([space]), []) if focus => {
            let first = reports::first(ctx, alongside.continue_turns(ctx).await)?;
            let code = space::run(ctx, space.clone(), execution, true, change).await?;
            Ok(first.max(code))
        }
        (Some([space]), []) if alone && queuing == Queuing::Here { now: true } => {
            space::run(ctx, space.clone(), execution, false, change).await
        }
        (Some([space]), []) if alone => {
            queue::queue_space(ctx, space.clone(), execution, queuing, change).await
        }
        (Some([]), [setup]) if focus => {
            let first = reports::first(ctx, alongside.continue_turns(ctx).await)?;
            let code = setups::retry_here(ctx, setup, change).await?;
            Ok(first.max(code))
        }
        _ => {
            let restore = Restore {
                execution,
                forget,
                queuing,
                change,
            };
            restore_each(ctx, selected, alongside, restore).await
        }
    }
}

/// Add each of the `reopened` workspaces with saved agents or last sessions
/// to the `selected` spaces, whose restore then restores them.
async fn select_reopened(
    ctx: &Context,
    selected: &mut Selected,
    reopened: &mut [Reopened],
) -> Result<()> {
    for opened in reopened.iter_mut().filter(|opened| opened.agents_saved) {
        let space = SpaceSelector::id(&opened.space.id);
        if crate::client::saved::read(&ctx.paths, space.clone())
            .await?
            .is_empty()
        {
            continue;
        }
        selected.spaces.get_or_insert_default().push(space);
        opened.agents_saved = false;
    }
    Ok(())
}

/// Restore what the `selected` spaces and the workspaces reopened for them
/// have but their agents, which stay saved: retry the failed setups,
/// leaving the agents waiting for them to the queue, and report them with
/// what it does `alongside`, or that there is nothing else to restore.
async fn restore_without_agents(
    ctx: &Context,
    selected: &Selected,
    alongside: Alongside,
) -> Result<i32> {
    if selected.setups.is_empty() {
        return reports::nothing(ctx, alongside.continue_turns(ctx).await, false);
    }
    let restored = set_up(ctx, selected, alongside.continue_turns(ctx).await, false).await;
    reports::queued(ctx, &restored)?;
    Ok(if restored.failed.is_empty() { 0 } else { 1 })
}

/// The turns an interrupt ended of the agents in the `selected` spaces and
/// running chats, taking those spaces and chats from the selection, which
/// keeps the others, and refusing a `changing` --permission, which a
/// running agent cannot take.
async fn interrupted_turns(
    ctx: &Context,
    selected: &mut Selected,
    changing: bool,
) -> Result<Continuation> {
    let Some(spaces) = selected.spaces.take() else {
        return Ok(Continuation::default());
    };
    let (mut continuation, rest) = Continuation::plan(ctx, spaces, Clone::clone).await?;
    selected.spaces = Some(rest);
    let running = std::mem::take(&mut selected.running);
    let (chats, idle) =
        Continuation::plan(ctx, running, |chat| SpaceSelector::id(&chat.id)).await?;
    selected.running = idle;
    continuation.extend(chats);
    ensure!(
        continuation.is_empty() || !changing,
        "an interrupted turn continues in its running agent, which keeps its permissions; restart the agent with --permission instead"
    );
    Ok(continuation)
}

/// Refuse a restore in a process below a scoped one that cleared its scope
/// token: a scoped process only queues the restores of the agents of the
/// spaces its scope reaches, which the daemon starts, or forgets their
/// stopped work.
pub(super) fn ensure_queued() -> Result<()> {
    // The daemon checks that each space is in the caller's scope, which
    // needs the scope token: a process that cleared it stays refused.
    ensure!(
        crate::env::is_scoped(),
        "scoped processes queue the restores of the spaces their scope reaches only with their scope token"
    );
    Ok(())
}

/// How [`restore_each`] restores the spaces it selects.
struct Restore<'a> {
    /// The one stopped execution of the one space selected to restore.
    execution: Option<ExecutionId>,
    forget: bool,
    queuing: Queuing,
    change: Option<&'a PermissionChange>,
}

/// Restore the `selected` spaces together, reporting them at once with
/// what it does `alongside`: retry the failed setups, and queue the
/// restores of the stopped agents and the agents waiting for those setups,
/// making `change` when given, start them as `queuing` says, or forget the
/// stopped work for `forget`.
async fn restore_each(
    ctx: &Context,
    selected: Selected,
    mut alongside: Alongside,
    restore: Restore<'_>,
) -> Result<i32> {
    let Restore {
        execution,
        forget,
        queuing,
        change,
    } = restore;
    let mut stopped = saved::stopped(ctx, &selected, forget).await?;
    if let Some(execution) = &execution {
        for (_, saved) in &mut stopped {
            saved.retain_execution(execution, true);
        }
    }
    if stopped.is_empty() && selected.setups.is_empty() {
        let started = forget || queuing == Queuing::Here { now: true };
        return reports::nothing(ctx, alongside.continue_turns(ctx).await, started);
    }
    if forget {
        return forget::forget_each(ctx, &stopped, alongside.reopened).await;
    }
    let mut refused = Vec::new();
    if let Queuing::Daemon { .. } = queuing {
        stopped = daemon_startable(ctx, stopped, &mut alongside.reopened, &mut refused).await?;
    }
    permission_change::apply(ctx, &stopped, &selected.setups, change).await?;
    let now = matches!(queuing, Queuing::Here { now } | Queuing::Daemon { now } if now);
    let mut restored = set_up(ctx, &selected, alongside.continue_turns(ctx).await, now).await;
    restored.failed.append(&mut refused);
    match queuing {
        Queuing::Here { now: true } => {
            let restored = StartedRestores {
                set_up: restored.set_up,
                reopened: restored.reopened,
                continued: restored.continued,
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

/// The `stopped` work whose restores the daemon can start from the
/// background, refusing the rest before any is queued, except that the
/// agents of a workspace `reopened` for the restore stay saved, reported
/// among the `failed`, since its reopening stands.
async fn daemon_startable(
    ctx: &Context,
    stopped: Vec<(Space, Saved)>,
    reopened: &mut [Reopened],
    failed: &mut Vec<Failure>,
) -> Result<Vec<(Space, Saved)>> {
    let mut startable = Vec::new();
    for (space, saved) in stopped {
        let starts = queue::ensure_daemon_starts_in(ctx, &space, &saved).await;
        let opened = (reopened.iter_mut()).find(|opened| opened.space.id == space.id);
        match (starts, opened) {
            (Err(error), Some(opened)) => {
                opened.agents_saved = true;
                failed.push(ui::failure(ctx, "restore", &space.name, &error));
            }
            (starts, _) => {
                starts?;
                startable.push((space, saved));
            }
        }
    }
    Ok(startable)
}

/// Retry the `selected` failed setups, starting the agents waiting for them
/// at once when `now`, going on after one fails, and report them with what
/// the restore did already, `restored`.
async fn set_up(ctx: &Context, selected: &Selected, mut restored: Queued, now: bool) -> Queued {
    let (set_up, mut failed) = setups::retry_each(ctx, &selected.setups, now).await;
    restored.set_up = set_up;
    restored.failed.append(&mut failed);
    restored
}

/// The space `selector` names, a workspace or a chat.
async fn present_space(ctx: &Context, selector: &SpaceSelector) -> Result<Space> {
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
