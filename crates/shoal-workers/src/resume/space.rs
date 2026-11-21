//! Choosing the stopped agent or last session of one space a resume
//! restores, a workspace's or a chat's, once nothing refuses its restore.
use anyhow::{Context as _, Result, ensure};
use shoal_core::execution::recovery::{Saved, SavedCommand, SavedSession};
use shoal_ids::execution_ids::ExecutionId;

use super::{Resuming, commands, permissions, picker, queue, stale};
use crate::{
    client::{
        connection::CommandPaths,
        output::resume::{Queued, StoppedCommands},
    },
    config::permissions::PermissionChange,
    model::Space,
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::status::Inspection;

/// What one space's stopped work comes to for a resume of it.
pub enum Choice {
    /// Only stopped commands, which the resume reported to the user.
    Commands(StoppedCommands),
    /// The queued restore of the chosen agent, started at once in the
    /// wrapper that waits for it.
    Queued(Queued),
    /// The chosen agent or last session, which the caller restores.
    Restore(Box<Restore>),
}

/// The stopped agent or last session `picked` of `space` that a resume
/// restores, with the permission change given made.
pub struct Restore {
    pub space: Space,
    pub picked: SavedSession,
    /// `picked` is the last session of an agent that exited, not a stopped
    /// agent.
    pub last_session: bool,
    /// The commands stopped with the space's agents, which its restore
    /// claims.
    pub commands: Vec<SavedCommand>,
}

/// Choose the stopped agent of `space` to restore, the `execution` one when
/// given, else its only one or one `resuming` picks, or else its last
/// session, making `change` when given; one whose wrapper waits to restore
/// it starts there at once.
pub async fn choose(
    paths: &CommandPaths,
    space: Space,
    execution: Option<&ExecutionId>,
    change: Option<&PermissionChange>,
    resuming: &impl Resuming,
) -> Result<Choice> {
    let mut saved = saved(paths, &space, execution).await?;
    if saved.agents.is_empty() && !saved.sessions.is_empty() {
        let picked = picker::select_agent(resuming, &saved.sessions, picker::session_label)?;
        let restore = Restore {
            space,
            picked,
            last_session: true,
            commands: saved.commands,
        };
        return restore.chosen(paths, change).await;
    }
    let recovery_id = execution.or_else(|| (saved.agents.len() == 1).then(|| &saved.agents[0].id));
    let active = active_executions(paths, &space, recovery_id).await?;
    if saved.agents.is_empty() {
        let report = commands::report_claimed(resuming, &space, &mut saved.commands)?;
        return Ok(Choice::Commands(report));
    }
    let picked = picker::select_agent(resuming, &saved.agents, picker::agent_label)?;
    if picked.waits_in_wrapper {
        saved.retain_execution(&picked.id, true);
        let stopped = vec![(space, saved)];
        return Ok(Choice::Queued(
            queue::queue_each(paths, stopped, true, resuming).await,
        ));
    }
    ensure!(
        !active.contains(&picked.id),
        "selected execution is active or unknown; stop or reconcile it first"
    );
    let restore = Restore {
        space,
        picked,
        last_session: false,
        commands: saved.commands,
    };
    restore.chosen(paths, change).await
}

impl Restore {
    /// This restore as a resume chooses it, once the daemon made `change`,
    /// when given, to the permissions its agent was given.
    async fn chosen(
        self,
        paths: &CommandPaths,
        change: Option<&PermissionChange>,
    ) -> Result<Choice> {
        permissions::apply_one(paths, &self.space, &self.picked.id, change).await?;
        Ok(Choice::Restore(Box::new(self)))
    }
}

/// Refuse `space` when it is a workspace whose setup failed or never
/// started, naming `shoal setup`, which retries it before the agent waiting
/// for it starts; a chat has no setup.
pub async fn refuse_failed_setup(paths: &CommandPaths, space: &Space) -> Result<()> {
    if !space.kind().has_worktree() {
        return Ok(());
    }
    let inspection =
        crate::client::workspaces::inspect(paths, SpaceSelector::id(&space.id)).await?;
    ensure!(
        !awaits_setup(&inspection),
        "the setup of {} failed; retry it with `shoal setup {}`",
        space.name,
        space.number
    );
    Ok(())
}

/// Whether the inspected workspace waits for a retried setup: its setup
/// failed, or never started and runs in no execution, as when the daemon
/// refused it.
fn awaits_setup(inspection: &Inspection) -> bool {
    match inspection.workspace.state {
        WorkspaceState::Failed => true,
        WorkspaceState::Preparing => inspection.executions.is_empty(),
        _ => false,
    }
}

/// The space `selector` names, a workspace or a chat.
pub async fn present_space(paths: &CommandPaths, selector: &SpaceSelector) -> Result<Space> {
    let space = crate::client::spaces::present_space(paths, selector).await?;
    space.with_context(|| format!("no space {selector}"))
}

/// What `space` saved, only of `execution` when given, refusing a space
/// with nothing to resume.
pub async fn saved(
    paths: &CommandPaths,
    space: &Space,
    execution: Option<&ExecutionId>,
) -> Result<Saved> {
    let mut saved = crate::client::saved::read(paths, SpaceSelector::id(&space.id)).await?;
    if let Some(execution) = execution {
        saved.retain_execution(execution, true);
    }
    ensure!(
        !saved.is_empty(),
        "no stopped agents or commands for this {}",
        space.kind()
    );
    Ok(saved)
}

/// The executions of `space` recorded once a daemon restart's unknown
/// `selected` one, when any, is proven ended, as a workspace's repair
/// proves it; a chat's agent registers only while none of it runs.
async fn active_executions(
    paths: &CommandPaths,
    space: &Space,
    selected: Option<&ExecutionId>,
) -> Result<Vec<ExecutionId>> {
    if space.worktree().is_none() {
        return Ok(Vec::new());
    }
    let selector = SpaceSelector::id(&space.id);
    let inspection = crate::client::workspaces::inspect(paths, selector.clone()).await?;
    stale::reconcile(paths, &inspection, selected).await?;
    let inspection = crate::client::workspaces::inspect(paths, selector).await?;
    Ok(inspection.executions.into_iter().map(|e| e.id).collect())
}
