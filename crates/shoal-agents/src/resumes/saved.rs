//! Restoring what a stop saved of a space: each stopped agent and last
//! session queues its restore, which the daemon's worker starts where the
//! agent stopped or in a background place, and stopped commands go to the
//! restored agent, or to the user when no agent restores outside the
//! wrapper that waits for it.
use anyhow::{Context as _, Result, ensure};

use crate::Agents;
use crate::daemon::Daemon;
use crate::execution::recovery::{self, Saved, SavedCommand, SavedExt as _};
use crate::model::{AgentResume, QueueEntry, ResumeOutcome, Space};
use crate::protocol::SavedAgent;
use crate::state::ExecutionState;
use crate::stopped_work::PermissionTargets;
use shoal_daemon_base::callers::Caller;
use shoal_protocol::space_names::SpaceSelector;

impl Agents {
    /// Restore what `space` saved, only `resume`'s execution when given,
    /// for `caller`, once nothing refuses any of it.
    pub(super) async fn restore_saved(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        space: &Space,
        resume: AgentResume,
    ) -> Result<ResumeOutcome> {
        let mut saved = Saved::load(&self.paths, &space.id)?;
        if let Some(execution) = &resume.execution {
            saved.retain_execution(execution, true);
        }
        if saved.is_empty() {
            let reason = format!("no stopped agents or commands for this {}", space.kind());
            return Ok(ResumeOutcome::Nothing { reason });
        }
        if saved.agents.is_empty() && saved.sessions.is_empty() {
            let commands = take_commands(&mut saved.commands)?;
            return Ok(ResumeOutcome::Commands { commands });
        }
        self.ensure_restores_start(daemon, space, &saved, &resume)
            .await?;
        self.ensure_stopped(daemon, space, &saved).await?;
        if let Some(change) = &resume.change {
            let agents = saved_agents(space, &saved);
            let targets = PermissionTargets {
                agents: &agents,
                launches: &[],
            };
            (self.change_stopped_permissions(daemon, targets, change, caller)).await?;
        }
        let entries = (self.queue_restores(daemon, caller, space, &saved, resume)).await?;
        let selector = SpaceSelector::id(&space.id);
        // A resume the user makes is their interaction with its agent.
        (self.record_user_interaction(daemon, caller, &selector)).await;
        let commands = match saved.restores_outside_wrappers() {
            true => Vec::new(),
            false => take_commands(&mut saved.commands)?,
        };
        Ok(ResumeOutcome::Queued { entries, commands })
    }

    /// Refuse the restores of `saved` that the daemon would start from the
    /// background when no host can take them there from the caller's place
    /// `resume` records; a waiting wrapper restores its agent where it
    /// stopped.
    async fn ensure_restores_start(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        saved: &Saved,
        resume: &AgentResume,
    ) -> Result<()> {
        if !saved.restores_outside_wrappers() {
            return Ok(());
        }
        let settings = daemon.space_settings(space).await?;
        crate::queued_starts::ensure_background_start(daemon, &settings, &resume.env)
            .with_context(|| {
                format!(
                    "cannot restore the agents of {} in the background; pass --focus to restore one here",
                    space.name
                )
            })
    }

    /// Refuse to restore an agent of `saved` whose execution still runs in
    /// `space`, and clear one a daemon restart or an unproven stop left
    /// unknown once its processes are proven gone; an agent whose wrapper
    /// waits to restore it is stopped, not running. A chat's agent registers
    /// only while none of it runs.
    async fn ensure_stopped(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        saved: &Saved,
    ) -> Result<()> {
        if space.worktree().is_none() {
            return Ok(());
        }
        let executions = self.space_executions(&space.id).await?;
        for session in saved.agents.iter().chain(&saved.sessions) {
            let Some(execution) = executions.iter().find(|e| e.id == session.id) else {
                continue;
            };
            ensure!(
                execution.state != ExecutionState::Running || execution.stopped.is_some(),
                "the agent {} of {} is active; stop it before restoring",
                session.id,
                space.name
            );
            if execution.state == ExecutionState::Unknown {
                let ended = daemon.prove_execution_ended(&space.id, &session.id);
                ensure!(
                    ended.await?,
                    "the agent {} of {} is still unresolved; stop what remains of it with shoal stop, then restore again",
                    session.id,
                    space.name
                );
            }
        }
        Ok(())
    }

    /// Queue the restore of each agent and last session `saved` keeps of
    /// `space`, starting it at once when `resume` asks for it; a last
    /// session, which holds no heavy slot, continues at once when the user
    /// resumes its space alone and no stopped agent restores with it, while
    /// a scoped `caller` starts nothing past the limits it did not ask for.
    async fn queue_restores(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        space: &Space,
        saved: &Saved,
        resume: AgentResume,
    ) -> Result<Vec<QueueEntry>> {
        let last_session_alone = resume.alone && caller.is_none() && saved.agents.is_empty();
        let now = resume.now || last_session_alone;
        let selector = SpaceSelector::id(&space.id);
        let mut entries = Vec::new();
        for session in saved.agents.iter().chain(&saved.sessions) {
            let (execution, env) = (&session.id, resume.env.clone());
            entries.push(match now {
                true => (self.start_stopped_restore(daemon, &selector, execution, env)).await?,
                false => (self.queue_stopped_restore(daemon, &selector, execution, env)).await?,
            });
        }
        Ok(entries)
    }
}

/// The agents and last sessions `saved` keeps of `space`, as a change to
/// the permissions they were given names them.
fn saved_agents(space: &Space, saved: &Saved) -> Vec<SavedAgent> {
    (saved.agents.iter().chain(&saved.sessions))
        .map(|session| SavedAgent {
            workspace: SpaceSelector::id(&space.id),
            execution: session.id.clone(),
        })
        .collect()
}

/// Take the stopped `commands` that no other restore holds, which go to the
/// user rather than to a restored agent, each as its arguments.
fn take_commands(commands: &mut Vec<SavedCommand>) -> Result<Vec<Vec<String>>> {
    let mut taken = Vec::new();
    for command in commands.drain(..) {
        // Another restore holding the command reports it.
        let Ok(_claim) = recovery::claim(&command.path) else {
            continue;
        };
        recovery::consume(&command.path)?;
        taken.push(command.argv);
    }
    Ok(taken)
}
