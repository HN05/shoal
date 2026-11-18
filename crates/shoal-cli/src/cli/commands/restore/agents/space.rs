//! Resuming one space's stopped agent or last session, a workspace's or a
//! chat's, in this terminal or where it stopped or in the background.
use anyhow::{Result, ensure};
use std::path::PathBuf;

use super::{changed, commands, permission_change, picker, queue, reports, sessions, stale};
use crate::client::output::restore::StartedRestores;
use crate::execution::recovery::RecordExt;
use crate::{
    cli::{context::Context, terminal},
    config::permissions::PermissionChange,
    execution::recovery::{
        LastSession, Recovery, Saved, SavedCommand, changes::Change, claim_last_session,
        claim_record,
    },
    model::Space,
    protocol::ConfigTarget,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_workers::execution::Restored;

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
    let mut saved = crate::client::saved::read(&ctx.paths, SpaceSelector::id(&space.id)).await?;
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
    space: &Space,
    selected: Option<&ExecutionId>,
) -> Result<Vec<ExecutionId>> {
    if space.worktree().is_none() {
        return Ok(Vec::new());
    }
    let selector = SpaceSelector::id(&space.id);
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
    space: &Space,
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
    space: &Space,
    session: LastSession,
    path: PathBuf,
    commands: Vec<SavedCommand>,
) -> Result<i32> {
    let handoff = commands::handoff(&commands);
    let mut records = vec![path];
    if let Some(change) = Change::of(&session) {
        records.extend(commands.into_iter().map(|command| command.path));
        let handoff = handoff.as_deref();
        return changed::start_changed(ctx, space, session, change, handoff, records).await;
    }
    let selector = SpaceSelector::id(&space.id);
    let home = shoal_workers::homes::records(&ctx.paths, &selector).await?;
    let restored = Restored {
        agent: &session.agent,
        model: &session.model,
        mode: &session.mode,
        narrowing: session.narrowing,
        session: session.session.as_ref(),
    };
    let recovery = shoal_workers::execution::resolve(
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

/// Start the restore command in `space`, consuming `records` once the agent
/// runs.
async fn launch(
    ctx: &Context,
    space: &Space,
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
    let owner = match space.to_workspace() {
        // A host that runs agents in places of its own runs a workspace's
        // there; a chat runs where its resume runs, as its start does.
        Some(workspace) => {
            let target = ConfigTarget::Space(SpaceSelector::id(&workspace.id));
            let settings = shoal_workers::settings::of(&ctx.paths, target).await?;
            let command = &recovery.command;
            let ran = terminal::run_agent(ctx, &settings, &workspace, &agent, &records, command);
            if let Some(code) = ran.await? {
                return Ok(code);
            }
            crate::terminal::Owner::Workspace(&space.id)
        }
        None => crate::terminal::Owner::Chat(&space.id),
    };
    terminal::label_current(&ctx.paths, owner).await?;
    shoal_workers::execution::run_recovery(
        &ctx.paths,
        SpaceSelector::id(&space.id),
        recovery.command,
        agent,
        records,
    )
    .await
}
