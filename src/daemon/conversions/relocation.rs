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
    /// Restart the agent of each pending conversion whose turn ended, each
    /// under an operation guard, so an update waits for it, and with the
    /// hand-overs held, so a stop of the workspace's agents, which hands its
    /// agent over too, comes before or after it. A conversion read again
    /// once they are held may be gone, its agent stopped.
    pub(crate) async fn relocate_finished_turns(&self) -> Result<()> {
        let pending = self.store.run(|db| super::record::all_pending(db)).await?;
        for id in pending.into_iter().map(|(id, _)| id) {
            let _operation = self.gates.operation().await;
            let _handovers = self.conversion_handovers.lock().await;
            let space = id.clone();
            let pending = self.store.run(move |db| super::pending(db, &space)).await?;
            let Some(pending) = pending else {
                continue;
            };
            if !self.turn_ended(&id, &pending).await? {
                continue;
            }
            let relocated = match pending.clone() {
                Pending::Workspace {
                    mode,
                    chat,
                    execution,
                } => self.relocate_to_workspace(&id, mode, chat, execution).await,
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
    /// a new workspace once it is ready, after its setup or the repair of a
    /// setup that failed.
    async fn turn_ended(&self, id: &SpaceId, pending: &Pending) -> Result<bool> {
        let space = id.to_owned();
        let state = self
            .store
            .run(move |db| crate::daemon::workspace::agent_state::current(db, &space))
            .await?;
        let idle = state.is_some_and(|status| status.state == AgentState::Idle);
        match pending {
            Pending::Workspace { chat, .. } => {
                // A setup that failed waits for its repair, which readies it.
                let workspace = self.workspace_by_id(id).await?;
                if workspace.state != WorkspaceState::Ready {
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
    /// row was `chat`, in the worktree in `mode`, as [`Self::hand_over`]
    /// keeps it as `execution`. A hand-over that fails leaves the agent
    /// running where it was and the conversion pending, to try again.
    async fn relocate_to_workspace(
        &self,
        id: &SpaceId,
        mode: ModeName,
        chat: ChatRow,
        execution: ExecutionId,
    ) -> Result<()> {
        let origin = chat.agent()?.origin()?;
        if let Err(error) = self.hand_over(id, mode, &chat, execution).await {
            log!("agent of {id} not handed over yet: {error:#}");
            return Ok(());
        }
        resume_workspace(&self.paths, id, origin.as_ref()).await
    }

    /// Hand the agent of a chat that became the workspace `id`, whose row was
    /// `chat`, over to the workspace, ahead of a stop of the workspace's
    /// agents, which finds it nowhere else: once its session is kept as the
    /// workspace's last session, which `shoal resume` continues, stop it
    /// where it ran, and forget the conversion. Whether it had such an agent.
    pub(crate) async fn stop_converting_agent(&self, id: &SpaceId) -> Result<bool> {
        // A workspace that becomes a chat stops its own agent, under the
        // hand-overs its relocation holds, so only a chat's agent takes them.
        if !self.converting_chat_agent(id).await? {
            return Ok(false);
        }
        let _handovers = self.conversion_handovers.lock().await;
        let space = id.to_owned();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        let Some(Pending::Workspace {
            mode,
            chat,
            execution,
        }) = pending
        else {
            return Ok(false);
        };
        self.hand_over(id, mode, &chat, execution).await?;
        Ok(true)
    }

    /// Whether a chat's agent has yet to restart in the workspace `id` it
    /// became.
    async fn converting_chat_agent(&self, id: &SpaceId) -> Result<bool> {
        let space = id.to_owned();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        Ok(matches!(pending, Some(Pending::Workspace { .. })))
    }

    /// Keep the session of the agent of the chat that became the workspace
    /// `id`, whose row was `chat`, as the workspace's last session under
    /// `execution`, marked converted, in `mode`, then stop the agent where it
    /// ran and forget the conversion and the scope the agent carried there.
    async fn hand_over(
        &self,
        id: &SpaceId,
        mode: ModeName,
        chat: &ChatRow,
        execution: ExecutionId,
    ) -> Result<()> {
        let agent = chat.agent()?;
        let last = LastSession {
            execution,
            agent: agent.agent.clone(),
            session: agent.session.clone(),
            moved: false,
            copied: None,
            converted: true,
            origin: agent.origin()?,
            stopped_by: None,
            stop: None,
            model: Default::default(),
            mode,
            narrowing: agent.narrowing()?,
        };
        last.write(&self.paths, id)?;
        self.stop_converted_chat(id, &agent.process()?).await?;
        let (space, token) = (id.to_owned(), agent.scope_token.clone());
        self.store
            .write(move |tx| super::record::relocated_chat(tx, &space, token.as_ref()))
            .await?;
        self.signals.agent_changed();
        Ok(())
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
