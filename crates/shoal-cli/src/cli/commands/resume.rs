//! `shoal resume`: bringing back the agents of the selected spaces,
//! workspaces and chats alike, which the modules below carry out. A stopped
//! agent's restore queues unless asked to start now, an asleep one's starts
//! as the queue admits it, a turn an interrupt ended continues, a last
//! session continues and a failed launch queues again; resumes use saved
//! agent identity and today's restore configuration, and a workspace whose
//! setup failed retries it before the agent waiting for it starts.
use crate::client::output::{
    batch::Failure,
    restore::Reopened,
    resume::{Queued, StartedRestores},
};
use crate::{
    cli::{args::SpaceTargets, commands::turns::Continuation, context::Context, ui},
    config::permissions::PermissionChange,
    execution::recovery::Saved,
    model::Space,
};
use anyhow::{Context as _, Result, ensure};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::saved::Queuing;
use shoal_protocol::space_names::SpaceSelector;

use targets::Selected;

/// How `shoal resume` brings back the agents of the spaces it selects.
pub struct Options {
    /// The one stopped execution of a space to resume.
    pub execution: Option<ExecutionId>,
    /// Start the agents at once, regardless of the agent limits.
    pub now: bool,
    /// Resume one agent in this terminal.
    pub focus: bool,
    /// What changes in the permissions the resumed agents were given;
    /// without it they keep them.
    pub change: Option<PermissionChange>,
}

/// `shoal resume`: bring back the agents of the spaces `targets` selects,
/// once each, as `options` says; without a terminal or anything to pick,
/// the spaces whose agent can resume are listed instead.
pub(super) async fn run(ctx: &Context, targets: SpaceTargets, options: Options) -> Result<i32> {
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        ensure_queued()?;
    }
    let named = !targets.space.is_empty();
    match targets::Selected::load(ctx, targets).await? {
        Some(selected) => resume(ctx, selected, named, options, Alongside::default()).await,
        None => Ok(0),
    }
}

/// Resume the agents of the spaces `restore` opened again, the `reopened`
/// ones whose removal saved any, as `options` says, reporting the reopened
/// spaces with them.
pub(in crate::cli::commands) async fn reopened(
    ctx: &Context,
    mut reopened: Vec<Reopened>,
    options: Options,
) -> Result<i32> {
    let mut selected = Selected {
        spaces: Some(Vec::new()),
        ..Selected::default()
    };
    select_reopened(ctx, &mut selected, &mut reopened).await?;
    let alongside = Alongside {
        reopened,
        turns: Continuation::default(),
    };
    resume(ctx, selected, false, options, alongside).await
}

/// Resume the agents of the `selected` spaces, every one with a stopped
/// agent for `--all`, with what the run does `alongside`, continuing the
/// turns an interrupt ended in the named ones as the rest resumes; `named`
/// says the command line named them. Restores queue unless `now`, or
/// `focus`, which resumes one agent in this terminal, and so do the agents
/// waiting for the failed setups of named workspaces, which retry them
/// first; a chat's start at once from the queue, which admits them without
/// a slot. Without a terminal every restore queues, which `now` starts at
/// once, since this process can start none of them. Every target, and a
/// change to the permissions given for every selected agent, is checked
/// before any setup, continued turn or restore.
async fn resume(
    ctx: &Context,
    mut selected: Selected,
    named: bool,
    options: Options,
    mut alongside: Alongside,
) -> Result<i32> {
    let change = options.change.as_ref();
    let whole = named && options.execution.is_none();
    if whole {
        setups::take_failed(ctx, &mut selected).await?;
    }
    // A failed launch resumes whether the command line named its space or a
    // picker chose it.
    if options.execution.is_none() {
        failed_starts::take(ctx, &mut selected, false).await?;
    }
    if whole {
        alongside.turns = interrupted_turns(ctx, &mut selected, change.is_some()).await?;
    }
    if selected.is_empty() && alongside.reopened.is_empty() && !alongside.turns.is_empty() {
        return alongside.turns.submit(ctx).await;
    }
    if let Some(change) = change {
        permission_change::check(ctx, &selected, change).await?;
    }
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        let now = options.now || options.focus;
        return queue::queue_below(ctx, selected, options.execution, change, now, alongside).await;
    }
    ensure_one_agent(&selected, &options)?;
    resume_selected(ctx, selected, alongside, options).await
}

/// What a resume does beside restoring stopped agents, which it reports
/// with them: the removed spaces `shoal restore` opened again for it, and
/// the turns an interrupt ended, which it continues once nothing is left to
/// refuse the resume.
#[derive(Default)]
pub(super) struct Alongside {
    reopened: Vec<Reopened>,
    turns: Continuation,
}

impl Alongside {
    /// Whether there is nothing to report beside the agents restored.
    fn is_empty(&self) -> bool {
        self.reopened.is_empty() && self.turns.is_empty()
    }

    /// Continue the interrupted turns, returning what to report: the
    /// reopened spaces, the continued turns and the failures to continue
    /// them.
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

/// Refuse `--execution`, which selects an agent of one space, and
/// `--focus`, which resumes one agent here, unless the `selected` spaces
/// come to one, or for `--focus` to one failed setup or launch.
fn ensure_one_agent(selected: &Selected, options: &Options) -> Result<()> {
    let Some(spaces) = &selected.spaces else {
        return Ok(());
    };
    ensure!(
        options.execution.is_none() || spaces.len() == 1,
        "--execution selects a stopped execution of one space"
    );
    let launches = selected.setups.len() + selected.failed_starts.len();
    ensure!(
        !options.focus || spaces.len() + launches == 1,
        "--focus resumes the agent of one space"
    );
    Ok(())
}

/// Resume the agents of the `selected` spaces as `options` says, with what
/// the run does `alongside`: one space's alone or in this terminal, the
/// agent waiting for one failed setup or launch in this terminal, or every
/// one together.
async fn resume_selected(
    ctx: &Context,
    selected: Selected,
    alongside: Alongside,
    options: Options,
) -> Result<i32> {
    let Options {
        execution,
        now,
        focus,
        change,
    } = options;
    let change = change.as_ref();
    let queuing = Queuing::of(ctx.on_terminal(), now);
    let setups = selected.setups.as_slice();
    // A space resumes alone only when nothing else, such as a failed
    // launch, resumes with it.
    let alone = alongside.is_empty() && selected.failed_starts.is_empty();
    match (selected.spaces.as_deref(), setups) {
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
        (Some([]), []) if focus && selected.failed_starts.len() == 1 => {
            let first = reports::first(ctx, alongside.continue_turns(ctx).await)?;
            let failed = &selected.failed_starts[0];
            let code = failed_starts::retry_here(ctx, failed, change).await?;
            Ok(first.max(code))
        }
        _ => {
            let resume = Resume {
                execution,
                queuing,
                change,
            };
            resume_each(ctx, selected, alongside, resume).await
        }
    }
}

/// Add each of the `reopened` spaces with saved agents or last sessions to
/// the `selected` spaces, whose resume then resumes them.
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

/// The turns an interrupt ended of the agents in the `selected` spaces,
/// workspaces and chats alike, taking those spaces from the selection,
/// which keeps the others, and refusing a `changing` --permission, which a
/// running agent cannot take.
async fn interrupted_turns(
    ctx: &Context,
    selected: &mut Selected,
    changing: bool,
) -> Result<Continuation> {
    let Some(spaces) = selected.spaces.take() else {
        return Ok(Continuation::default());
    };
    let (continuation, rest) = Continuation::plan(ctx, spaces, Clone::clone).await?;
    selected.spaces = Some(rest);
    ensure!(
        continuation.is_empty() || !changing,
        "an interrupted turn continues in its running agent, which keeps its permissions; restart the agent with --permission instead"
    );
    Ok(continuation)
}

/// Refuse a resume in a process below a scoped one that cleared its scope
/// token: a scoped process only queues the restores of the agents of the
/// spaces its scope reaches, which the daemon starts, or discards their
/// stopped agents.
pub(in crate::cli::commands) fn ensure_queued() -> Result<()> {
    // The daemon checks that each space is in the caller's scope, which
    // needs the scope token: a process that cleared it stays refused.
    ensure!(
        crate::env::is_scoped(),
        "scoped processes queue the restores of the spaces their scope reaches only with their scope token"
    );
    Ok(())
}

/// How [`resume_each`] resumes the spaces it selects.
struct Resume<'a> {
    /// The one stopped execution of the one space selected to resume.
    execution: Option<ExecutionId>,
    queuing: Queuing,
    change: Option<&'a PermissionChange>,
}

/// Resume the `selected` spaces together, reporting them at once with what
/// the run does `alongside`: retry the failed setups, and queue the
/// restores of the stopped agents and the agents waiting for those setups,
/// making `change` when given, and start them as `queuing` says.
async fn resume_each(
    ctx: &Context,
    selected: Selected,
    mut alongside: Alongside,
    resume: Resume<'_>,
) -> Result<i32> {
    let Resume {
        execution,
        queuing,
        change,
    } = resume;
    let mut stopped = saved::stopped(ctx, &selected, false).await?;
    if let Some(execution) = &execution {
        for (_, saved) in &mut stopped {
            saved.retain_execution(execution, true);
        }
    }
    if stopped.is_empty() && selected.setups.is_empty() && selected.failed_starts.is_empty() {
        let started = queuing == Queuing::Here { now: true };
        return reports::nothing(ctx, alongside.continue_turns(ctx).await, started);
    }
    let mut refused = Vec::new();
    if let Queuing::Daemon { .. } = queuing {
        stopped = daemon_startable(ctx, stopped, &mut alongside.reopened, &mut refused).await?;
    }
    let launches = (
        selected.setups.as_slice(),
        selected.failed_starts.as_slice(),
    );
    permission_change::apply(ctx, &stopped, launches, change).await?;
    let now = matches!(queuing, Queuing::Here { now } | Queuing::Daemon { now } if now);
    let mut resumed = set_up(ctx, &selected, alongside.continue_turns(ctx).await, now).await;
    retry_failed_starts(ctx, &selected, &mut resumed, now).await;
    resumed.failed.append(&mut refused);
    match queuing {
        Queuing::Here { now: true } => {
            let resumed = StartedRestores {
                queued: resumed.queued,
                set_up: resumed.set_up,
                reopened: resumed.reopened,
                continued: resumed.continued,
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

/// The `stopped` work whose restores the daemon can start from the
/// background, refusing the rest before any is queued, except that the
/// agents of a space `reopened` for the resume stay saved, reported among
/// the `failed`, since its reopening stands.
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
                failed.push(ui::failure(ctx, "resume", &space.name, &error));
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
/// the resume did already, `resumed`.
async fn set_up(ctx: &Context, selected: &Selected, mut resumed: Queued, now: bool) -> Queued {
    let (set_up, mut failed) = setups::retry_each(ctx, &selected.setups, now).await;
    resumed.set_up = set_up;
    resumed.failed.append(&mut failed);
    resumed
}

/// Queue the `selected` failed launches again, starting them at once when
/// `now`, going on after one fails, and report them in `resumed`.
async fn retry_failed_starts(ctx: &Context, selected: &Selected, resumed: &mut Queued, now: bool) {
    let failed_starts = &selected.failed_starts;
    let (mut retried, mut failed) = failed_starts::retry_each(ctx, failed_starts, now).await;
    resumed.queued.append(&mut retried);
    resumed.failed.append(&mut failed);
}

/// The space `selector` names, a workspace or a chat.
async fn present_space(ctx: &Context, selector: &SpaceSelector) -> Result<Space> {
    let space = crate::client::workspaces::present_space(&ctx.paths, selector).await?;
    space.with_context(|| format!("no space {selector}"))
}

pub(in crate::cli::commands) mod changed;
mod commands;
pub(in crate::cli::commands) mod failed_starts;
pub(in crate::cli::commands) mod permission_change;
mod picker;
mod places;
pub(in crate::cli::commands) mod queue;
pub(in crate::cli::commands) mod queued;
pub(in crate::cli::commands) mod reports;
pub(in crate::cli::commands) mod saved;
mod sessions;
pub(in crate::cli::commands) mod setups;
mod space;
mod stale;
pub(in crate::cli::commands) mod targets;
