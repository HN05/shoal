//! Restarting a converted space's agent in the new place: once its turn
//! ends, Shoal stops it where the space was and starts it again from a fork
//! of its session, in the new workspace's worktree or the chat's checkout,
//! told what the space is now.
use crate::client::agent::AgentSessionId;
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result};

use super::{Pending, record::ChatRow, record::ConvertedChat};
use crate::protocol::operations::{Ended, Failure};
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

/// How a stop of a converting workspace's agents settles its conversion.
#[derive(Clone, Copy)]
pub(crate) struct ConversionStop<'a> {
    /// The space whose process stops them; none for the user or Shoal.
    pub stopped_by: Option<&'a SpaceId>,
    /// Whether the stop keeps them down until `shoal resume`, which keeps the
    /// chat a conversion makes of the workspace stopped too.
    pub holds: bool,
}

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
    /// under an operation guard, so an update waits for it. A chat's agent
    /// restarts in its workspace, and a workspace becomes a chat, with the
    /// hand-overs held until the new agent is admitted, so a stop of the
    /// workspace's agents, which takes them too, comes before, and the
    /// relocation reads the conversion as that stop left it, or after the
    /// new agent runs, which it stops.
    pub(crate) async fn relocate_finished_turns(&self) -> Result<()> {
        let pending = self.store.run(|db| super::record::all_pending(db)).await?;
        #[cfg(test)]
        tests::LISTED.notify_one();
        for id in pending.into_iter().map(|(id, _)| id) {
            let _operation = self.gates.operation().await;
            let handovers = &self.conversion_handovers;
            handovers.hold(self.relocate(&id)).await?;
        }
        Ok(())
    }

    /// Restart the agent of the conversion of the space `id`, read again
    /// under the hand-overs, once its turn ended.
    async fn relocate(&self, id: &SpaceId) -> Result<()> {
        let space = id.clone();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        let Some(pending) = pending else {
            return Ok(());
        };
        if !self.turn_ended(id, &pending).await? {
            return Ok(());
        }
        let relocated = match pending.clone() {
            Pending::Workspace {
                mode,
                chat,
                execution,
                preparation,
            } => match self.failed_hook(preparation.as_deref()).await {
                Some(failure) => {
                    (self.stop_after_failed_hook(id, mode, chat, execution, &failure)).await
                }
                None => self.relocate_to_workspace(id, mode, chat, execution).await,
            },
            Pending::Chat { chat, held } => self.relocate_to_chat(id, *chat, held).await,
        };
        if let Err(error) = relocated {
            self.report_failed_relocation(id, &pending, &error).await;
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
            Pending::Workspace {
                chat, preparation, ..
            } => {
                // A setup that failed waits for its repair, which readies it.
                let workspace = self.workspace_by_id(id).await?;
                if workspace.state != WorkspaceState::Ready
                    || !self.preparation_over(preparation.as_deref()).await
                {
                    return Ok(false);
                }
                let process = chat.agent()?.process()?;
                Ok(idle || !identity::alive(&process)?)
            }
            Pending::Chat { chat, .. } => {
                let running = self.running_agents().await?;
                Ok(idle || !running.iter().any(|agent| agent.ran_as(&chat.execution)))
            }
        }
    }

    /// Whether the preparation `operation` of a chat's new workspace is over,
    /// post-setup hook included. One no longer followed ended long before.
    async fn preparation_over(&self, operation: Option<&str>) -> bool {
        let Some(operation) = operation else {
            return true;
        };
        match self.operation(operation, None).await {
            Ok(entry) => entry.events_from(u64::MAX).1.is_some(),
            Err(_) => true,
        }
    }

    /// Why the post-setup hook of the preparation `operation` of a chat's new
    /// workspace failed, after which a start runs no agent there either.
    async fn failed_hook(&self, operation: Option<&str>) -> Option<String> {
        let entry = self.operation(operation?, None).await.ok()?;
        match entry.events_from(u64::MAX).1 {
            Some(Ended::Failed(Failure::HookFailed { message })) => Some(message),
            _ => None,
        }
    }

    /// Stop the agent of the chat that became the workspace `id`, whose row
    /// was `chat`, as a stop of the workspace's agents does, keeping its
    /// session as `execution` for `shoal resume` in `mode`, since the
    /// workspace's post-setup hook failed with `failure`, and tell the user.
    async fn stop_after_failed_hook(
        &self,
        id: &SpaceId,
        mode: ModeName,
        chat: ChatRow,
        execution: ExecutionId,
        failure: &str,
    ) -> Result<()> {
        self.hand_over(id, mode, &chat, execution, None).await?;
        let name = self.workspace_by_id(id).await?.name;
        let message = format!(
            "The post-setup hook of {name} failed: {failure}; its agent stopped instead of restarting there, and shoal resume {name} continues it once the hook is repaired"
        );
        let about = About::space(&name, id);
        (self.notify_about(about, NotificationKind::RecoveryBlocked, message)).await;
        Ok(())
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
        if let Err(error) = self.hand_over(id, mode, &chat, execution, None).await {
            log!("agent of {id} not handed over yet: {error:#}");
            return Ok(());
        }
        resume_workspace(&self.paths, id, origin.as_ref()).await
    }

    /// Settle the conversion of the workspace `id` ahead of a stop of its
    /// agents as `stop` says, with the hand-overs held: a stop that keeps
    /// them down holds a conversion into a chat, and the agent of a chat that
    /// became the workspace, which the stop finds nowhere else, is handed
    /// over to it, once its session is kept as the workspace's last session,
    /// which `shoal resume` continues. Whether it had such an agent.
    pub(crate) async fn stop_converting_agent(
        &self,
        id: &SpaceId,
        stop: ConversionStop<'_>,
    ) -> Result<bool> {
        let handovers = &self.conversion_handovers;
        handovers.hold(self.settle_for_stop(id, stop)).await
    }

    /// [`Self::stop_converting_agent`] once the hand-overs are held.
    async fn settle_for_stop(&self, id: &SpaceId, stop: ConversionStop<'_>) -> Result<bool> {
        if stop.holds {
            self.hold_converting_chat(id, stop.stopped_by).await?;
        }
        let space = id.to_owned();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        let Some(Pending::Workspace {
            mode,
            chat,
            execution,
            ..
        }) = pending
        else {
            return Ok(false);
        };
        self.hand_over(id, mode, &chat, execution, stop.stopped_by)
            .await?;
        Ok(true)
    }

    /// Hand over the agent of a chat that became the workspace `id` ahead of
    /// its removal, as [`Self::stop_converting_agent`] does, also within a
    /// relocation's removal of the spaces above it.
    pub(crate) async fn hand_over_before_removal(&self, id: &SpaceId) -> Result<()> {
        let space = id.to_owned();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        if let Some(Pending::Workspace { .. }) = pending {
            let stop = ConversionStop {
                stopped_by: None,
                holds: false,
            };
            self.stop_converting_agent(id, stop).await?;
        }
        Ok(())
    }

    /// Keep `session` as the one the agent of the chat that became the
    /// workspace `id` reported last, which its hand-over continues; a
    /// workspace without such an agent keeps none.
    pub(crate) async fn record_converting_session(
        &self,
        id: &SpaceId,
        session: AgentSessionId,
    ) -> Result<()> {
        let id = id.to_owned();
        (self.store)
            .run(move |db| super::record::record_session(db, &id, &session))
            .await
    }

    /// Keep the chat that the workspace `id` becomes stopped, as a stop of
    /// its agent by the process of the space `stopped_by`, or by the user,
    /// keeps that agent down until `shoal resume`.
    async fn hold_converting_chat(&self, id: &SpaceId, stopped_by: Option<&SpaceId>) -> Result<()> {
        let (id, stopped_by) = (id.to_owned(), stopped_by.cloned());
        (self.store)
            .run(move |db| super::record::hold_chat(db, &id, stopped_by.as_ref()))
            .await
    }

    /// Keep the session of the agent of the chat that became the workspace
    /// `id`, whose row was `chat`, as the workspace's last session under
    /// `execution`, marked converted, in `mode`, stopped by the process of
    /// the space `stopped_by` when given, then stop the agent where it
    /// ran, keeping what each round signals for a hand-over that was
    /// interrupted to go on stopping, and forget the conversion and the
    /// scope the agent carried there.
    async fn hand_over(
        &self,
        id: &SpaceId,
        mode: ModeName,
        chat: &ChatRow,
        execution: ExecutionId,
        stopped_by: Option<&SpaceId>,
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
            stopped_by: stopped_by.cloned(),
            stop: None,
            model: Default::default(),
            mode,
            narrowing: agent.narrowing()?,
        };
        last.write(&self.paths, id)?;
        let space = id.to_owned();
        let stopped = (self.store)
            .run(move |db| super::record::stopped_processes(db, &space))
            .await?;
        let record = |signalled: Vec<_>| {
            let space = id.to_owned();
            self.store
                .write(move |tx| super::record::record_stopped(tx, &space, &signalled))
        };
        (self.stop_converted_chat(id, &agent.process()?, stopped, record)).await?;
        let (space, token) = (id.to_owned(), agent.scope_token.clone());
        self.store
            .write(move |tx| super::record::relocated_chat(tx, &space, token.as_ref()))
            .await?;
        self.signals.agent_changed();
        Ok(())
    }

    /// Make the workspace `id` the stopped `chat` its agent carries on in:
    /// stop the agent, remove the worktree as `shoal rm` does, and restore
    /// the chat in the repository's checkout, continuing the session the
    /// agent reported last, whether it still runs or exited, unless a stop
    /// `held` it, which leaves the chat for `shoal resume`.
    async fn relocate_to_chat(&self, id: &SpaceId, chat: ConvertedChat, held: bool) -> Result<()> {
        let chat = ConvertedChat {
            session: self.latest_session(id, &chat),
            ..chat
        };
        self.convert_worktree_to_chat(id, &chat).await?;
        if held {
            return Ok(());
        }
        let chat = self
            .chat_by_id(id)
            .await?
            .context("the chat the workspace became is gone")?;
        self.restore_converted_chat(&chat).await
    }

    /// The session the agent `chat` carries on reported last in the workspace
    /// `id`: while it runs, else as its last session kept it when it exited,
    /// else as the conversion recorded it.
    fn latest_session(&self, id: &SpaceId, chat: &ConvertedChat) -> Option<AgentSessionId> {
        let execution = &chat.execution;
        recovery::reported_session(&self.paths, id, execution)
            .or_else(|| {
                let path = LastSession::path(&self.paths, id, execution);
                LastSession::load(&path).ok().flatten()?.session
            })
            .or_else(|| chat.session.clone())
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
