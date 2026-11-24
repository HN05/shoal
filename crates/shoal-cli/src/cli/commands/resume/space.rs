//! Resuming one space's stopped agent or last session, a workspace's or a
//! chat's, in this terminal, as `--focus` and the daemon's start of a
//! queued restore do.
use anyhow::{Result, ensure};
use shoal_workers::hosting;
use std::path::PathBuf;

use super::changed;
use super::reports::{self, Reporting};
use crate::execution::recovery::{LastSessionExt as _, RecordExt};
use crate::{
    cli::context::Context,
    config::permissions::PermissionChange,
    execution::recovery::{
        LastSession, Recovery, SavedCommand, changes::Change, claim_last_session, claim_record,
    },
    model::Space,
    protocol::ConfigTarget,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_workers::execution::Restored;
use shoal_workers::resume::{
    self, commands,
    space::{Choice, Restore},
};

/// Resume the stopped agent of the space `space` names in this terminal,
/// the `execution` one when given, making `change` when given; one whose
/// wrapper waits to restore it starts there at once.
pub(super) async fn here(
    ctx: &Context,
    space: SpaceSelector,
    execution: Option<ExecutionId>,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let space = present_space(ctx, &space).await?;
    let reporting = Reporting(ctx);
    let execution = execution.as_ref();
    let choice = resume::space::choose(&ctx.paths, space, execution, change, &reporting);
    match choice.await? {
        Choice::Commands(report) => {
            reports::commands_only(ctx, &report)?;
            Ok(0)
        }
        Choice::Queued(queued) => reports::resumed(ctx, &queued),
        Choice::Restore(chosen) if chosen.last_session => continue_last_session(ctx, *chosen).await,
        Choice::Restore(chosen) => restore_agent(ctx, *chosen).await,
    }
}

/// The space `selector` names, a workspace or a chat.
pub(super) async fn present_space(ctx: &Context, selector: &SpaceSelector) -> Result<Space> {
    resume::space::present_space(&ctx.paths, selector).await
}

/// Restore the stopped agent `chosen` picked in this terminal, telling it
/// about its stop and the stopped commands.
async fn restore_agent(ctx: &Context, chosen: Restore) -> Result<i32> {
    let Restore {
        space,
        picked,
        mut commands,
        ..
    } = chosen;
    // The restore claims the commands it reports, which a restore started
    // elsewhere claims for itself.
    let _commands = commands::claim(&mut commands);
    let (_claim, recorded) = claim_record(&picked.path)?;
    if let Some(reason) = &recorded.stop_reason {
        reports::restoring(&recorded.agent, reason);
    }
    let session = recorded.into_last_session(picked.id, &picked.path);
    restore(ctx, &space, session, picked.path, commands).await
}

/// Continue the last session `chosen` picked in this terminal, telling it
/// about the stopped commands.
async fn continue_last_session(ctx: &Context, chosen: Restore) -> Result<i32> {
    let Restore {
        space,
        picked,
        mut commands,
        ..
    } = chosen;
    let (_claim, last) = claim_last_session(&picked.path)?;
    let _commands = commands::claim(&mut commands);
    if !last.moved {
        reports::continuing(&last.agent);
    }
    restore(ctx, &space, last, picked.path, commands).await
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
        session.ending(),
    )
    .await?;
    if !commands.is_empty() && !recovery.handoff_delivered {
        reports::commands_not_told(space, &commands);
    }
    records.extend(commands.into_iter().map(|command| command.path));
    launch(ctx, space, recovery, records).await
}

/// Start the restore command in `space`, where its terminal host runs
/// agents, otherwise here, consuming `records` once the agent runs.
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
    // A host that runs agents in places of its own runs the space's there.
    let target = ConfigTarget::Space(SpaceSelector::id(&space.id));
    let settings = shoal_workers::settings::of(&ctx.paths, target).await?;
    let placed = hosting::AgentSpace::of(space);
    let command = &recovery.command;
    let source = restore_source();
    let restore = (records.as_slice(), source.is_some());
    let ran = hosting::run_agent(ctx.caller(), &settings, &placed, &agent, restore, command);
    if let Some(code) = ran.await? {
        return Ok(code);
    }
    hosting::label_current(&ctx.paths, placed.owner()).await?;
    shoal_workers::execution::run_recovery(
        &ctx.paths,
        SpaceSelector::id(&space.id),
        recovery.command,
        agent,
        (records, source),
    )
    .await
}

/// What made this restore: the user, who runs it here, unless the queue
/// started it for an admitted entry, whose interaction its queuing decided.
fn restore_source() -> Option<shoal_protocol::launches::LaunchSource> {
    (crate::env::queue_entry().is_none()).then_some(shoal_protocol::launches::LaunchSource::Manual)
}

#[cfg(test)]
mod tests;
