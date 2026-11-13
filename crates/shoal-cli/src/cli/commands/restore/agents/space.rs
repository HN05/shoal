//! Resuming one space's stopped agent or last session, a workspace's or a
//! chat's, in this terminal or where it stopped or in the background.
use anyhow::{Context as _, Result, ensure};
use std::path::PathBuf;

use super::{commands, permission_change, picker, queue, reports, sessions, stale};
use crate::client::execution_ids::ExecutionId;
use crate::client::output::restore::StartedRestores;
use crate::client::space_names::SpaceSelector;
use crate::execution::recovery::RecordExt;
use crate::{
    cli::{context::Context, terminal},
    config::permissions::PermissionChange,
    execution::recovery::{
        Copied, LastSession, Recovery, Restored, Saved, SavedCommand, claim_last_session,
        claim_record,
    },
    model::{PresentSpace, Workspace},
    protocol::ConfigTarget,
};

/// Resume the stopped agent of the space `space` names, the `execution` one
/// when given. The agent restores in this terminal when `here`, otherwise
/// where it stopped or in the background, making `change` when given; one
/// whose wrapper waits to restore it starts there at once.
pub(super) async fn run(
    ctx: &Context,
    space: SpaceSelector,
    execution: Option<ExecutionId>,
    here: bool,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let space = super::present_space(ctx, &space).await?;
    let mut saved = crate::client::saved::read(&ctx.paths, SpaceSelector::id(space.id())).await?;
    if let Some(execution) = &execution {
        saved.retain_execution(execution, true);
    }
    ensure!(
        !saved.is_empty(),
        "no stopped agents or commands for this {}",
        space.kind()
    );
    if saved.agents.is_empty() && !saved.sessions.is_empty() {
        return continue_last_session(ctx, &space, saved, here, change).await;
    }
    let recovery_id = execution
        .as_ref()
        .or_else(|| (saved.agents.len() == 1).then(|| &saved.agents[0].id));
    let active = active_executions(ctx, &space, recovery_id).await?;
    if saved.agents.is_empty() {
        let _commands = commands::claim(&mut saved.commands);
        let report = commands::report(ctx, &space, &saved.commands)?;
        reports::commands_only(ctx, &report)?;
        return Ok(0);
    }
    let picked = picker::select_agent(ctx, &saved.agents, picker::agent_label)?;
    let (selected_id, selected) = (picked.id.clone(), picked.path.clone());
    if picked.waits_in_wrapper {
        saved.retain_execution(&selected_id, true);
        let stopped = vec![(space, saved)];
        return queue::queue_all(ctx, stopped, Default::default(), true).await;
    }
    ensure!(
        !active.contains(&selected_id),
        "selected execution is active or unknown; stop or reconcile it first"
    );
    permission_change::apply_one(ctx, &space, &selected_id, change).await?;
    if !here {
        let session = (space, picked);
        return sessions::start_sessions(ctx, vec![session], true, StartedRestores::default())
            .await;
    }
    // The restore claims the commands it reports, which a restore started
    // elsewhere claims for itself.
    let _commands = commands::claim(&mut saved.commands);
    let (_claim, recorded) = claim_record(&selected)?;
    if let Some(reason) = &recorded.stop_reason {
        reports::restoring(&recorded.agent, reason);
    }
    let session = recorded.into_last_session(selected_id, &selected);
    restore(ctx, &space, session, selected, saved.commands).await
}

/// The executions of `space` recorded once a daemon restart's unknown
/// `selected` one, when any, is proven ended, as a workspace's repair
/// proves it; a chat's agent registers only while none of it runs.
async fn active_executions(
    ctx: &Context,
    space: &PresentSpace,
    selected: Option<&ExecutionId>,
) -> Result<Vec<ExecutionId>> {
    let PresentSpace::Workspace(workspace) = space else {
        return Ok(Vec::new());
    };
    let selector = SpaceSelector::id(&workspace.id);
    let inspection = crate::client::workspaces::inspect(&ctx.paths, selector.clone()).await?;
    stale::reconcile(ctx, &inspection, selected).await?;
    let inspection = crate::client::workspaces::inspect(&ctx.paths, selector).await?;
    Ok(inspection.executions.into_iter().map(|e| e.id).collect())
}

/// Continue one of the last sessions that `saved` keeps of `space`, the
/// only one or one picked on a terminal, telling it about the stopped
/// commands: in this terminal when `here`, otherwise where it stopped or in
/// the background, making `change` when given.
pub(super) async fn continue_last_session(
    ctx: &Context,
    space: &PresentSpace,
    saved: Saved,
    here: bool,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let picked = picker::select_agent(ctx, &saved.sessions, picker::session_label)?;
    permission_change::apply_one(ctx, space, &picked.id, change).await?;
    if !here {
        let session = (space.clone(), picked);
        return sessions::start_sessions(ctx, vec![session], true, StartedRestores::default())
            .await;
    }
    let (_claim, last) = claim_last_session(&picked.path)?;
    let mut commands = saved.commands;
    let _commands = commands::claim(&mut commands);
    if !last.moved {
        reports::continuing(&last.agent);
    }
    restore(ctx, space, last, picked.path, commands).await
}

/// Restore the agent of `session` in `space`, saved at `path`, telling it
/// about its stop and the stopped `commands`, which the caller claimed; it
/// consumes their records once it runs.
async fn restore(
    ctx: &Context,
    space: &PresentSpace,
    session: LastSession,
    path: PathBuf,
    commands: Vec<SavedCommand>,
) -> Result<i32> {
    let handoff = commands::handoff(&commands);
    let mut records = vec![path];
    let changed = session.converted || session.copied.is_some() || session.moved;
    match (space, changed) {
        (PresentSpace::Workspace(workspace), true) => {
            records.extend(commands.into_iter().map(|command| command.path));
            return start_changed(ctx, workspace, session, handoff.as_deref(), records).await;
        }
        // A chat changes only by a workspace becoming it.
        (PresentSpace::Chat(chat), true) => {
            records.extend(commands.into_iter().map(|command| command.path));
            let start = crate::cli::commands::chat::start_converted(ctx, chat, &session, records);
            return start.await;
        }
        (_, false) => {}
    }
    let selector = SpaceSelector::id(space.id());
    let home = crate::agents::HomeRecords::read(&ctx.paths, &selector).await?;
    let restored = Restored {
        agent: &session.agent,
        model: &session.model,
        mode: &session.mode,
        narrowing: session.narrowing,
        session: session.session.as_ref(),
    };
    let recovery = Recovery::resolve(
        &ctx.paths,
        home.home()?,
        restored,
        handoff.as_deref(),
        session.stop.as_ref(),
    )
    .await?;
    if !commands.is_empty() && !recovery.handoff_delivered {
        reports::commands_not_told(space, &commands);
    }
    records.extend(commands.into_iter().map(|command| command.path));
    launch(ctx, space, recovery, records).await
}

/// Start the agent of `session` in `workspace`, which changed since it
/// stopped: converted from a chat, copied from another workspace or moved,
/// told so; it consumes `records` once it has started.
async fn start_changed(
    ctx: &Context,
    workspace: &Workspace,
    session: LastSession,
    handoff: Option<&str>,
    records: Vec<PathBuf>,
) -> Result<i32> {
    if session.converted {
        return start_converted(ctx, workspace, &session, handoff, records).await;
    }
    if let Some(copied) = session.copied {
        return start_copied(ctx, workspace, &session, copied, handoff, records).await;
    }
    start_moved(ctx, workspace, &session, handoff, records).await
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
    let settings = crate::config::settings(
        &ctx.paths,
        ConfigTarget::Space(SpaceSelector::id(&workspace.id)),
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
/// instructions, and about the stopped commands `handoff` describes; it
/// consumes `records` once it has started.
async fn start_converted(
    ctx: &Context,
    workspace: &Workspace,
    session: &LastSession,
    handoff: Option<&str>,
    records: Vec<PathBuf>,
) -> Result<i32> {
    let agent = &session.agent;
    let launch: crate::agent::Agent = agent
        .name()
        .parse()
        .with_context(|| format!("cannot start agent {agent} of {}", workspace.name))?;
    let settings = crate::config::settings(
        &ctx.paths,
        ConfigTarget::Space(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let fork = session
        .session
        .clone()
        .filter(|_| crate::agents::ensure_forks(&settings, agent).is_ok());
    let subject = crate::execution::recovery::converted_subject(workspace, fork.is_some(), handoff);
    let options = crate::cli::agents::LaunchOptions {
        replaces: records,
        fork,
        model: crate::cli::agents::ModelRequest::Kept(session.model.clone()),
        narrowing: session.narrowing,
    };
    let opening = crate::cli::agents::Opening::New {
        mode: session.mode.clone(),
        subject: Some(subject),
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

/// Start the restore command in `space`, consuming `records` once the agent
/// runs.
async fn launch(
    ctx: &Context,
    space: &PresentSpace,
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
            ctx.on_terminal(),
            "session picker requires a terminal; configure [agents.{}] resume_command for noninteractive recovery",
            recovery.agent
        );
    }
    let agent = recovery.tracked();
    let owner = match space {
        // A host that runs agents in places of its own runs a workspace's
        // there; a chat runs where its resume runs, as its start does.
        PresentSpace::Workspace(workspace) => {
            let target = ConfigTarget::Space(SpaceSelector::id(&workspace.id));
            let settings = crate::config::settings(&ctx.paths, target).await?;
            let command = &recovery.command;
            let ran = terminal::run_agent(ctx, &settings, workspace, &agent, &records, command);
            if let Some(code) = ran.await? {
                return Ok(code);
            }
            crate::terminal::Owner::Workspace(&workspace.id)
        }
        PresentSpace::Chat(chat) => crate::terminal::Owner::Chat(&chat.id),
    };
    terminal::label_current(&ctx.paths, owner).await?;
    crate::execution::run_recovery(
        &ctx.paths,
        SpaceSelector::id(space.id()),
        recovery.command,
        agent,
        records,
    )
    .await
}
