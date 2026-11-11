//! Restarting a converted space's agent in the new place: once its turn
//! ends, Shoal stops it where the space was and starts it again from a fork
//! of its session, in the new workspace's worktree or the chat's checkout,
//! told what the space is now.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result};

use super::{Pending, record::ChatRow, record::ConvertedChat};
use crate::{
    config::modes::ModeName,
    daemon::{
        log,
        notifications::{About, NotificationKind},
        workspace::Manager,
    },
    execution::recovery::{self, LastSession, LastSessionExt},
    process::identity,
    sessions::Origin,
    state::{AgentState, WorkspaceState},
};

/// How often the pending conversions are looked at besides when an agent's
/// state changes, for a workspace whose setup finished meanwhile.
const RECHECK: Duration = Duration::from_secs(5);

/// Restart the agents of converted spaces in their new places as their
/// turns end, one after another.
pub(crate) async fn run(manager: Arc<Manager>) {
    let mut agents = manager.signals.watch_agents();
    loop {
        if let Err(error) = manager.relocate_finished_turns().await {
            log!("conversions: {error:#}");
        }
        tokio::select! {
            _ = agents.changed() => {}
            () = tokio::time::sleep(RECHECK) => {}
        }
    }
}

impl Manager {
    /// Restart the agent of each pending conversion whose turn ended.
    pub(crate) async fn relocate_finished_turns(&self) -> Result<()> {
        let pending = self.store.run(|db| super::record::all_pending(db)).await?;
        for (id, pending) in pending {
            if !self.turn_ended(&id, &pending).await? {
                continue;
            }
            let relocated = match pending.clone() {
                Pending::Workspace { mode, chat } => {
                    self.relocate_to_workspace(&id, mode, chat).await
                }
                Pending::Chat { mode, execution } => {
                    self.relocate_to_chat(&id, mode, execution).await
                }
            };
            if let Err(error) = relocated {
                self.report_failed_relocation(&id, &pending, &error).await;
            }
        }
        Ok(())
    }

    /// Whether the agent of the space `id` that `pending` converts ended its
    /// turn, or its process its run, and the new place is ready to take it:
    /// a new workspace once its setup is over.
    async fn turn_ended(&self, id: &SpaceId, pending: &Pending) -> Result<bool> {
        let space = id.to_owned();
        let state = self
            .store
            .run(move |db| crate::daemon::workspace::agent_state::current(db, &space))
            .await?;
        let idle = state.is_some_and(|status| status.state == AgentState::Idle);
        match pending {
            Pending::Workspace { chat, .. } => {
                let workspace = self.workspace_by_id(id).await?;
                if workspace.state == WorkspaceState::Preparing {
                    return Ok(false);
                }
                let process = chat.agent()?.process()?;
                Ok(idle || !identity::alive(&process)?)
            }
            Pending::Chat { execution, .. } => {
                let running = self.running_agents().await?;
                Ok(idle || !running.iter().any(|agent| agent.ran_as(execution)))
            }
        }
    }

    /// Restart the agent of the chat that became the workspace `id`, whose
    /// row was `chat`, in the worktree in `mode`: stop it where it ran, keep
    /// its session as the workspace's last session, and continue that.
    async fn relocate_to_workspace(
        &self,
        id: &SpaceId,
        mode: ModeName,
        chat: ChatRow,
    ) -> Result<()> {
        let agent = chat.agent()?;
        self.stop_converted_chat(id, &agent.process()?).await?;
        let origin = agent.origin()?;
        let last = LastSession {
            execution: ExecutionId::generate(),
            agent: agent.agent.clone(),
            session: agent.session.clone(),
            moved: false,
            copied: None,
            converted: true,
            origin: origin.clone(),
            stopped_by: None,
            stop: None,
            model: Default::default(),
            mode,
            narrowing: agent.narrowing()?,
        };
        last.write(&self.paths, id)?;
        let (space, token) = (id.to_owned(), agent.scope_token.clone());
        self.store
            .write(move |tx| {
                if let Some(token) = token {
                    tx.execute("DELETE FROM workspace_scopes WHERE token=?1", [token])?;
                }
                super::record::forget(tx, &space)
            })
            .await?;
        self.signals.agent_changed();
        resume_workspace(&self.paths, id, origin.as_ref()).await
    }

    /// Make the workspace `id` the chat its agent, which `execution` runs,
    /// carries on in, in `mode`: stop the agent, remove the worktree as
    /// `shoal rm` does, and restore the chat in the repository's checkout.
    async fn relocate_to_chat(
        &self,
        id: &SpaceId,
        mode: ModeName,
        execution: ExecutionId,
    ) -> Result<()> {
        let chat = self.converted_chat(id, mode, execution).await?;
        self.convert_worktree_to_chat(id, &chat).await?;
        let chat = self
            .chat_by_id(id)
            .await?
            .context("the chat the workspace became is gone")?;
        self.restore_converted_chat(&chat).await
    }

    /// The stopped chat the workspace `id` becomes in `mode`, whose agent
    /// `execution` runs, with the session it reported.
    async fn converted_chat(
        &self,
        id: &SpaceId,
        mode: ModeName,
        execution: ExecutionId,
    ) -> Result<ConvertedChat> {
        let recorded = self
            .workspace_executions(id)
            .await?
            .into_iter()
            .find(|recorded| recorded.id == execution)
            .context("the agent the workspace converts for is gone")?;
        let agent = recorded
            .agent
            .context("the execution the workspace converts for runs no agent")?;
        let process = recorded
            .wrapper
            .context("the agent the workspace converts for has no process")?;
        Ok(ConvertedChat {
            agent,
            session: recovery::reported_session(&self.paths, id, &execution),
            origin: recorded.origin,
            process,
            mode,
            narrowing: recorded.narrowing,
            execution,
        })
    }

    /// Tell the user that the agent of the space `id` did not restart in
    /// the new place `pending` names, and forget the conversion, which is
    /// not tried again.
    async fn report_failed_relocation(
        &self,
        id: &SpaceId,
        pending: &Pending,
        error: &anyhow::Error,
    ) {
        log!("space {id} not relocated: {error:#}");
        let space = id.to_owned();
        if let Err(error) = self
            .store
            .run(move |db| super::record::forget(db, &space))
            .await
        {
            log!("conversion of {id} not forgotten: {error:#}");
        }
        let name = match self
            .space(&crate::client::space_names::SpaceSelector::id(id))
            .await
        {
            Ok(space) => space.name().clone(),
            Err(_) => return,
        };
        let message = match pending {
            Pending::Workspace { .. } => format!(
                "The agent of {name} did not restart in its new worktree: {error:#}; continue it with shoal resume {name}"
            ),
            Pending::Chat { .. } => format!(
                "{name} did not become a chat: {error:#}; it stays a workspace, whose stopped agent shoal resume {name} continues"
            ),
        };
        self.notify_about(
            About::space(&name, id),
            NotificationKind::RecoveryBlocked,
            message,
        )
        .await;
    }
}

/// Continue the last session of the workspace `id` as `shoal resume` does,
/// where its agent ran at `origin` or in a background place.
#[cfg(not(test))]
async fn resume_workspace(
    paths: &crate::paths::Paths,
    id: &SpaceId,
    origin: Option<&Origin>,
) -> Result<()> {
    use std::process::Stdio;

    let mut argv = crate::workers::shoal_argv(paths)?;
    argv.extend(["resume".into(), id.to_string().into()]);
    let env = crate::terminal::detached_start_environment(origin);
    let output = crate::workers::detached_command(&argv)
        .envs(&env)
        .current_dir(&paths.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("run shoal resume")?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

/// Tests record the workspaces resumed instead of running this binary.
#[cfg(test)]
async fn resume_workspace(
    _paths: &crate::paths::Paths,
    id: &SpaceId,
    _origin: Option<&Origin>,
) -> Result<()> {
    tests::RESUMED.lock().unwrap().push(id.clone());
    Ok(())
}

#[cfg(test)]
pub(super) mod tests;
