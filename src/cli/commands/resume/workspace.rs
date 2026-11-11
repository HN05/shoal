//! Resuming one workspace's stopped agent or last session, in this terminal
//! or where it stopped or in the background.
use anyhow::{Context as _, Result, ensure};
use std::path::PathBuf;

use super::{commands, permission_change, picker, queue, reports, sessions, stale};
use crate::client::execution_ids::ExecutionId;
use crate::client::output::resume::Resumed;
use crate::client::space_names::SpaceSelector;
use crate::execution::recovery::{LastSessionExt, RecordExt, waits_in_wrapper};
use crate::{
    cli::{context::Context, terminal},
    config::permissions::PermissionChange,
    execution::recovery::{
        Copied, LastSession, Record, Recovery, Restored, Saved, SavedCommand, claim,
    },
    model::Workspace,
    protocol::ConfigTarget,
};

/// Resume `workspace`'s stopped agent, the `execution` one when given. The
/// agent restores in this terminal when `here`, otherwise where it stopped
/// or in the background, making `change` when given; one whose wrapper
/// waits to restore it starts there at once.
pub(super) async fn run(
    ctx: &Context,
    workspace: SpaceSelector,
    execution: Option<ExecutionId>,
    here: bool,
    change: Option<&PermissionChange>,
) -> Result<i32> {
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
        reports::commands_only(ctx, &report)?;
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
        reports::restoring(&recorded.agent, reason);
    }
    let session = recorded.into_last_session(selected_id, &selected);
    restore(ctx, workspace, session, selected, saved.commands).await
}

/// Continue one of the workspace's last sessions that `saved` keeps, the
/// only one or one picked on a terminal, telling it about the stopped
/// commands: in this terminal when `here`, otherwise where it stopped or in
/// the background, making `change` when given.
pub(super) async fn continue_last_session(
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
        reports::continuing(&last.agent);
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
    if session.converted {
        records.extend(commands.into_iter().map(|command| command.path));
        return start_converted(ctx, workspace, &session, records).await;
    }
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
        reports::commands_not_told(workspace, &commands);
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
    reports::starting_moved(&session.agent);
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
    reports::starting_copied(agent, fork.is_some());
    let message = crate::execution::recovery::copied_prompt(copied, fork.is_some(), handoff);
    start_told(ctx, workspace, session, fork, message, records).await
}

/// Start the agent of `session` in a workspace that was a chat: from a fork
/// of the session it reported there, or as a new agent when it reported none
/// or cannot start from a fork, told what the space is now and its mode's
/// instructions; it consumes `records` once it has started.
async fn start_converted(
    ctx: &Context,
    workspace: &Workspace,
    session: &LastSession,
    records: Vec<PathBuf>,
) -> Result<i32> {
    let agent = &session.agent;
    let launch: crate::agent::Agent = agent
        .name()
        .parse()
        .with_context(|| format!("cannot start agent {agent} of {}", workspace.name))?;
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let fork = session
        .session
        .clone()
        .filter(|_| crate::agents::ensure_forks(&settings, agent).is_ok());
    let message = crate::execution::recovery::converted_prompt(workspace, fork.is_some());
    let options = crate::cli::agents::LaunchOptions {
        replaces: records,
        fork,
        model: crate::cli::agents::ModelRequest::Kept(session.model.clone()),
        narrowing: session.narrowing,
    };
    let opening = crate::cli::agents::Opening::New {
        mode: session.mode.clone(),
        subject: Some(crate::config::templates::Subject::told(message)),
    };
    crate::cli::agents::launch_agent(ctx, launch, workspace.clone(), opening, vec![], options).await
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
