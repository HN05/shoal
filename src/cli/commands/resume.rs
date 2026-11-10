//! Manual recovery uses saved agent identity and today's restore configuration.
//! It queues restores unless asked to start them now, and restores the
//! stopped chats at once.
use crate::client::execution_ids::ExecutionId;
use crate::client::output::resume::{Queued, Resumed};
use crate::client::space_names::SpaceSelector;
use crate::execution::recovery::LastSessionExt;
use crate::execution::recovery::RecordExt;
use crate::execution::recovery::waits_in_wrapper;
use crate::{
    cli::{args::SpaceTargets, commands::turns::Continuation, context::Context, terminal},
    config::permissions::PermissionChange,
    execution::recovery::{
        Copied, LastSession, Record, Recovery, Restored, Saved, SavedCommand, claim,
    },
    model::Workspace,
    protocol::ConfigTarget,
};
use anyhow::{Context as _, Result, ensure};
use queue::Queuing;
use std::path::PathBuf;
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
        ([], Some([workspace])) if discard || focus || queuing == Queuing::Here { now: true } => {
            run(ctx, workspace.clone(), execution, discard, focus, change).await
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

/// Report that no space has anything to resume, as a resume that starts
/// agents at once when `started`, otherwise as one that queues them.
fn report_nothing(ctx: &Context, started: bool) -> Result<i32> {
    let message = "No stopped agents, commands or chats";
    match started {
        true => ctx.emit(message, Resumed::default())?,
        false => ctx.emit(message, Queued::default())?,
    }
    Ok(0)
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
        return report_nothing(ctx, discard || queuing == Queuing::Here { now: true });
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

/// Resume `workspace`'s stopped agent, the `execution` one when given, or
/// forget its stopped work for `discard`. The agent restores in this
/// terminal when `here`, otherwise where it stopped or in the background,
/// making `change` when given; one whose wrapper waits to restore it
/// starts there at once.
pub async fn run(
    ctx: &Context,
    workspace: SpaceSelector,
    execution: Option<ExecutionId>,
    discard: bool,
    here: bool,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    if discard {
        return discard::discard_work(ctx, workspace, execution).await;
    }
    let mut inspection = crate::client::workspaces::inspect(&ctx.paths, workspace).await?;
    let workspace_id = inspection.workspace.id.clone();
    let mut saved = Saved::load(&ctx.paths, &workspace_id)?;
    if let Some(execution) = &execution {
        saved.retain_execution(execution, true);
    }
    ensure!(
        !saved.is_empty(),
        "no stopped agents or commands for this workspace"
    );
    if saved.agents.is_empty() && !saved.sessions.is_empty() {
        let workspace = &inspection.workspace;
        return continue_last_session(ctx, workspace, saved, here, change).await;
    }
    let recovery_id = execution
        .as_ref()
        .or_else(|| (saved.agents.len() == 1).then(|| &saved.agents[0].0));
    stale::reconcile(ctx, &inspection, recovery_id).await?;
    inspection =
        crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace_id)).await?;
    let workspace = &inspection.workspace;
    let active = |id: &ExecutionId| inspection.executions.iter().any(|e| e.id == *id);
    if saved.agents.is_empty() {
        let _commands = commands::claim(&mut saved.commands);
        let report = commands::report(ctx, workspace, &saved.commands)?;
        ctx.show(&report, |_| {})?;
        return Ok(0);
    }
    let (selected_id, selected) = picker::select_agent(ctx, &saved.agents, picker::agent_label)?;
    if waits_in_wrapper(&selected) {
        saved.retain_execution(&selected_id, true);
        let stopped = vec![(workspace.clone(), saved)];
        return queue::queue_all(ctx, stopped, Vec::new(), Vec::new(), true).await;
    }
    ensure!(
        !active(&selected_id),
        "selected execution is active or unknown; stop or reconcile it first"
    );
    permission_change::apply_one(ctx, workspace, &selected_id, change).await?;
    if !here {
        let session = (workspace.clone(), selected_id, selected);
        return sessions::start_sessions(ctx, vec![session], true, Resumed::default()).await;
    }
    // The restore claims the commands it reports, which a restore started
    // elsewhere claims for itself.
    let _commands = commands::claim(&mut saved.commands);
    let _claim = claim(&selected)?;
    let recorded = Record::load(&selected)?.context("recovery record was already consumed")?;
    if let Some(reason) = &recorded.stop_reason {
        eprintln!("shoal: restoring {}, stopped: {reason}", recorded.agent);
    }
    let session = recorded.into_last_session(selected_id, &selected);
    restore(ctx, workspace, session, selected, saved.commands).await
}

/// Continue one of the workspace's last sessions that `saved` keeps, the
/// only one or one picked on a terminal, telling it about the stopped
/// commands: in this terminal when `here`, otherwise where it stopped or in
/// the background, making `change` when given.
async fn continue_last_session(
    ctx: &Context,
    workspace: &Workspace,
    saved: Saved,
    here: bool,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let (id, path) = picker::select_agent(ctx, &saved.sessions, picker::session_label)?;
    permission_change::apply_one(ctx, workspace, &id, change).await?;
    if !here {
        let session = (workspace.clone(), id, path);
        return sessions::start_sessions(ctx, vec![session], true, Resumed::default()).await;
    }
    let _claim = claim(&path)?;
    let last = LastSession::load(&path)?.context("last session was already continued")?;
    let mut commands = saved.commands;
    let _commands = commands::claim(&mut commands);
    if !last.moved {
        eprintln!("shoal: continuing the last {} session", last.agent);
    }
    restore(ctx, workspace, last, path, commands).await
}

/// Restore the agent of `session`, saved at `path`, telling it about its stop
/// and the stopped `commands`, which the caller claimed; it consumes their
/// records once it runs.
async fn restore(
    ctx: &Context,
    workspace: &Workspace,
    session: LastSession,
    path: PathBuf,
    commands: Vec<SavedCommand>,
) -> Result<i32> {
    let handoff = commands::handoff(&commands);
    let mut records = vec![path];
    if let Some(copied) = session.copied {
        records.extend(commands.into_iter().map(|command| command.path));
        return start_copied(
            ctx,
            workspace,
            &session,
            copied,
            handoff.as_deref(),
            records,
        )
        .await;
    }
    if session.moved {
        records.extend(commands.into_iter().map(|command| command.path));
        return start_moved(ctx, workspace, &session, handoff.as_deref(), records).await;
    }
    let restored = Restored {
        agent: &session.agent,
        model: &session.model,
        mode: &session.mode,
        narrowing: session.narrowing,
        session: session.session.as_ref(),
    };
    let recovery = Recovery::resolve(
        &ctx.paths,
        workspace,
        restored,
        handoff.as_deref(),
        session.stop.as_ref(),
    )
    .await?;
    if !commands.is_empty() && !recovery.handoff_delivered {
        eprintln!("{}", commands::command_list(workspace, &commands));
    }
    records.extend(commands.into_iter().map(|command| command.path));
    launch(ctx, workspace, recovery, records).await
}

/// Start the agent of `session` as a new agent is started, with the model
/// and effort it ran with, since its session is not available where the
/// workspace moved, telling it the workspace moved; it consumes `records`
/// once it has started.
async fn start_moved(
    ctx: &Context,
    workspace: &Workspace,
    session: &LastSession,
    handoff: Option<&str>,
    records: Vec<PathBuf>,
) -> Result<i32> {
    eprintln!(
        "shoal: starting a new {} session in the moved workspace",
        session.agent
    );
    let message = crate::execution::recovery::moved_prompt(handoff);
    start_told(ctx, workspace, session, None, message, records).await
}

/// Start the agent of `session` in a workspace copied from another: from a
/// fork of the session it reported there, which stays the original's, or as
/// a new agent when it reported none or cannot start from a fork, telling it
/// the workspace is a copy; it consumes `records` once it has started.
async fn start_copied(
    ctx: &Context,
    workspace: &Workspace,
    session: &LastSession,
    copied: Copied,
    handoff: Option<&str>,
    records: Vec<PathBuf>,
) -> Result<i32> {
    let agent = &session.agent;
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let fork = session
        .session
        .clone()
        .filter(|_| crate::agents::ensure_forks(&settings, agent).is_ok());
    match fork {
        Some(_) => eprintln!("shoal: starting {agent} from a fork of its session in the copy"),
        None => eprintln!("shoal: starting a new {agent} session in the copy"),
    }
    let message = crate::execution::recovery::copied_prompt(copied, fork.is_some(), handoff);
    start_told(ctx, workspace, session, fork, message, records).await
}

/// Start the agent of `session` as a new agent is started, from a fork of
/// `fork` when given, with the model and effort it ran with, told `message`;
/// it consumes `records` once it has started.
async fn start_told(
    ctx: &Context,
    workspace: &Workspace,
    session: &LastSession,
    fork: Option<crate::client::agent::AgentSessionId>,
    message: String,
    records: Vec<PathBuf>,
) -> Result<i32> {
    let agent = &session.agent;
    let launch: crate::agent::Agent = agent
        .name()
        .parse()
        .with_context(|| format!("cannot start agent {agent} of {}", workspace.name))?;
    let options = crate::cli::agents::LaunchOptions {
        replaces: records,
        fork,
        model: crate::cli::agents::ModelRequest::Kept(session.model.clone()),
        narrowing: session.narrowing,
    };
    let opening = crate::cli::agents::Opening::Told {
        mode: session.mode.clone(),
        message,
    };
    crate::cli::agents::launch_agent(ctx, launch, workspace.clone(), opening, vec![], options).await
}

/// Start the restore command, consuming `records` once the agent runs.
async fn launch(
    ctx: &Context,
    workspace: &Workspace,
    recovery: Recovery,
    records: Vec<PathBuf>,
) -> Result<i32> {
    ensure!(
        !recovery.command.is_empty(),
        "configure [agents.{}] resume_command with a session restore command",
        recovery.agent
    );
    if !recovery.automatic {
        ensure!(
            ctx.interactive(),
            "session picker requires a terminal; configure [agents.{}] resume_command for noninteractive recovery",
            recovery.agent
        );
    }
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let agent = recovery.tracked();
    if let Some(code) = terminal::run_agent(
        ctx,
        &settings,
        workspace,
        &agent,
        &records,
        &recovery.command,
    )
    .await?
    {
        return Ok(code);
    }
    terminal::label_current(&ctx.paths, crate::terminal::Owner::Workspace(&workspace.id)).await?;
    crate::execution::run_recovery(
        &ctx.paths,
        SpaceSelector::Id(workspace.id.clone()),
        recovery.command,
        agent,
        records,
    )
    .await
}

mod chats;
mod commands;
mod discard;
pub(super) mod permission_change;
mod picker;
pub(super) mod queue;
pub(super) mod queued;
mod sessions;
mod stale;
mod targets;

#[cfg(test)]
mod tests;
