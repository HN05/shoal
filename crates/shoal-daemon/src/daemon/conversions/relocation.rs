//! Restarting a converted space's agent in the new place: once its turn
//! ends, Shoal stops it where the space was and starts it again from a fork
//! of its session, in the new workspace's worktree or the chat's checkout,
//! told what the space is now.
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use anyhow::Result;

use super::{
    Pending,
    record::{ChatRow, ConvertedChat},
};
use crate::protocol::operations::{Ended, Failure};
use crate::{
    agent::Agent,
    config::modes::ModeName,
    daemon::{
        log,
        notifications::{About, NotificationKind},
        workspace::Manager,
    },
    execution::recovery::{self, LastSession, LastSessionExt},
    sessions::Origin,
    state::WorkspaceState,
};

/// How a stop of a converting workspace's agents settles its conversion.
#[derive(Clone, Copy)]
pub(crate) struct ConversionStop {
    /// Whether the stop keeps them down until `shoal restore`, which keeps the
    /// chat a conversion makes of the workspace stopped too.
    pub holds: bool,
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
        if !self.ready_to_relocate(id, &pending).await? {
            return Ok(());
        }
        // Read again once the turn ended, with the reports that change it,
        // its agent's sessions and its preparation's end, held off.
        let space = id.clone();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        let Some(pending) = pending else {
            return Ok(());
        };
        let relocated = match pending.clone() {
            Pending::Workspace {
                mode,
                chat,
                execution,
                hook_failure,
                ..
            } => match hook_failure {
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
        // An agent kept down or not restarted no longer works its issues.
        self.agent_settles.request(id);
        Ok(())
    }

    /// Whether the agent of the space `id` that `pending` converts ended its
    /// turn, or its process its run, and the new place is ready to take it:
    /// a new workspace once it is ready, after its setup or the repair of a
    /// setup that failed.
    async fn ready_to_relocate(&self, id: &SpaceId, pending: &Pending) -> Result<bool> {
        match pending {
            Pending::Workspace {
                preparation,
                prepared,
                ..
            } => {
                // A setup that failed waits for its repair, which readies it.
                let workspace = self.workspace_by_id(id).await?;
                if workspace.state != WorkspaceState::Ready
                    || !(*prepared || self.preparation_over(preparation.as_deref()).await)
                {
                    return Ok(false);
                }
                self.turn_ended(id, |agent| agent.space_id == *id).await
            }
            Pending::Chat { chat, .. } => {
                (self.turn_ended(id, |agent| agent.ran_as(&chat.execution))).await
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

    /// Keep with the conversion of the workspace `id` that its preparation
    /// `operation` reported it `ended`, beyond the operation's own record,
    /// which goes some time after it ends and with the daemon. A workspace
    /// without such a conversion keeps nothing.
    pub(crate) async fn keep_preparation_end(&self, id: &SpaceId, operation: &str, ended: &Ended) {
        let hook_failure = match ended {
            Ended::Failed(Failure::HookFailed { message }) => Some(message.clone()),
            _ => None,
        };
        let (space, operation) = (id.to_owned(), operation.to_owned());
        let kept = (self.store).run(move |db| {
            super::record::record_prepared(db, &space, &operation, hook_failure.as_deref())
        });
        // Under the hand-overs, so no relocation acts on the end it misses.
        if let Err(error) = self.conversion_handovers.hold(kept).await {
            log!("preparation end of {id} not kept: {error:#}");
        }
    }

    /// Stop the agent of the chat that became the workspace `id`, whose row
    /// was `chat`, as a stop of the workspace's agents does, keeping its
    /// session as `execution` for `shoal restore` in `mode`, since the
    /// workspace's post-setup hook failed with `failure`, and tell the user.
    /// A hand-over that fails leaves the conversion pending, to try again.
    async fn stop_after_failed_hook(
        &self,
        id: &SpaceId,
        mode: ModeName,
        chat: ChatRow,
        execution: ExecutionId,
        failure: &str,
    ) -> Result<()> {
        if let Err(error) = self.hand_over(id, mode, &chat, execution).await {
            log!("agent of {id} not handed over yet: {error:#}");
            return Ok(());
        }
        let workspace = self.workspace_by_id(id).await?;
        let (name, number) = (workspace.name, workspace.number);
        let message = format!(
            "The post-setup hook of {name} failed: {failure}; its agent stopped instead of restarting there, and shoal restore {number} continues it once the hook is repaired"
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
        let origin = match self.hand_over(id, mode, &chat, execution).await {
            Ok(origin) => origin,
            Err(error) => {
                log!("agent of {id} not handed over yet: {error:#}");
                return Ok(());
            }
        };
        restore_workspace(&self.paths, id, origin.as_ref()).await
    }

    /// Settle the conversion of the workspace `id` ahead of a stop of its
    /// agents as `stop` says, with the hand-overs held: a stop that keeps
    /// them down holds a conversion into a chat, and the agent of a chat that
    /// became the workspace, which the stop finds nowhere else, is handed
    /// over to it, once its session is kept as the workspace's last session,
    /// which `shoal restore` continues. Whether it had such an agent.
    pub(crate) async fn stop_converting_agent(
        &self,
        id: &SpaceId,
        stop: ConversionStop,
    ) -> Result<bool> {
        let handovers = &self.conversion_handovers;
        handovers.hold(self.settle_for_stop(id, stop)).await
    }

    /// [`Self::stop_converting_agent`] once the hand-overs are held.
    async fn settle_for_stop(&self, id: &SpaceId, stop: ConversionStop) -> Result<bool> {
        if stop.holds {
            self.hold_converting_chat(id).await?;
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
        self.hand_over(id, mode, &chat, execution).await?;
        Ok(true)
    }

    /// Hand over the agent of a chat that became the workspace `id` ahead of
    /// its removal, as [`Self::stop_converting_agent`] does, also within a
    /// relocation's removal of the spaces above it.
    pub(crate) async fn hand_over_before_removal(&self, id: &SpaceId) -> Result<()> {
        let space = id.to_owned();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        if let Some(Pending::Workspace { .. }) = pending {
            let stop = ConversionStop { holds: false };
            self.stop_converting_agent(id, stop).await?;
        }
        Ok(())
    }

    /// Keep the chat that the workspace `id` becomes stopped, as a stop of
    /// its agent keeps that agent down until `shoal restore`.
    async fn hold_converting_chat(&self, id: &SpaceId) -> Result<()> {
        let id = id.to_owned();
        (self.store)
            .run(move |db| super::record::hold_chat(db, &id))
            .await
    }

    /// Keep the session of the agent of the chat that became the workspace
    /// `id`, whose row was `chat`, which runs or ran as `execution`, as the
    /// workspace's last session under that execution, marked converted, in
    /// `mode`, then stop the agent where it ran, and forget the conversion
    /// and the scope the agent carried there. Returns where the agent ran in
    /// a terminal host, when it ran in one.
    async fn hand_over(
        &self,
        id: &SpaceId,
        mode: ModeName,
        chat: &ChatRow,
        execution: ExecutionId,
    ) -> Result<Option<Origin>> {
        let row = chat.agent()?;
        let ran = self.converting_agent(id, &execution).await?;
        if let Some((agent, origin)) = &ran {
            let last = LastSession {
                session: self.latest_session(id, &execution),
                execution,
                agent: agent.clone(),
                moved: false,
                copied: None,
                converted: true,
                origin: origin.clone(),
                stop: None,
                model: Default::default(),
                mode,
                narrowing: row.narrowing()?,
            };
            last.write(&self.paths, id)?;
        }
        // The workspace runs nothing else before the agent moves there.
        let request = crate::daemon::executions::StopRequest::conversion();
        (self.stop_space_agent_executions(id, &request)).await?;
        let (space, token) = (id.to_owned(), row.scope_token.clone());
        self.store
            .write(move |tx| super::record::relocated_chat(tx, &space, token.as_ref()))
            .await?;
        self.signals.agent_changed();
        Ok(ran.and_then(|(_, origin)| origin))
    }

    /// The agent that runs or ran as `execution` in the space `id`, with
    /// where it ran in a terminal host: as its execution records it while
    /// it runs, else as its last session kept it when it exited; none once
    /// neither does.
    async fn converting_agent(
        &self,
        id: &SpaceId,
        execution: &ExecutionId,
    ) -> Result<Option<(Agent, Option<Origin>)>> {
        let executions = self.workspace_executions(id).await?;
        let running = (executions.into_iter()).find(|recorded| recorded.id == *execution);
        if let Some(recorded) = running
            && let Some(agent) = recorded.agent
        {
            return Ok(Some((agent, recorded.origin)));
        }
        let path = LastSession::path(&self.paths, id, execution);
        Ok(LastSession::load(&path)?.map(|last| (last.agent, last.origin)))
    }

    /// Make the workspace `id` the stopped `chat` its agent carries on in:
    /// stop the agent, remove the worktree as `shoal rm` does, and restore
    /// the chat's agent in the repository's checkout through the agent
    /// queue, continuing the session the agent reported last, whether it
    /// still runs or exited, told what the space is now, unless a stop
    /// `held` it, which leaves the chat for `shoal restore`.
    async fn relocate_to_chat(&self, id: &SpaceId, chat: ConvertedChat, held: bool) -> Result<()> {
        let session = self.latest_session(id, &chat.execution);
        self.convert_worktree_to_chat(id, &chat).await?;
        let last = LastSession {
            execution: chat.execution.clone(),
            agent: chat.agent.clone(),
            session,
            moved: false,
            copied: None,
            converted: true,
            origin: chat.origin.clone(),
            stop: None,
            model: Default::default(),
            mode: chat.mode.clone(),
            narrowing: chat.narrowing,
        };
        last.write(&self.paths, id)?;
        if held {
            return Ok(());
        }
        let env = crate::terminal::detached_start_environment(chat.origin.as_ref());
        let selector = shoal_protocol::space_names::SpaceSelector::id(id);
        (self
            .start_stopped_restore(&selector, &chat.execution, env)
            .await)
            .map(drop)
    }

    /// The session the agent that runs or ran as `execution` in the space
    /// `id` reported last: while it runs, else as its last session kept it
    /// when it exited.
    fn latest_session(&self, id: &SpaceId, execution: &ExecutionId) -> Option<AgentSessionId> {
        recovery::reported_session(&self.paths, id, execution).or_else(|| {
            let path = LastSession::path(&self.paths, id, execution);
            LastSession::load(&path).ok().flatten()?.session
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
        // The hints select the space by its number, which it keeps as the
        // other kind, since names select nothing.
        let (name, number) = match self
            .space(&shoal_protocol::space_names::SpaceSelector::id(id))
            .await
        {
            Ok(space) => (space.name.clone(), space.number),
            Err(_) => return,
        };
        let message = match pending {
            Pending::Workspace { .. } => format!(
                "The agent of {name} did not restart in its new worktree: {error:#}; continue it with shoal restore {number}"
            ),
            Pending::Chat { .. } if self.chat_by_id(id).await.is_ok_and(|chat| chat.is_some()) => {
                format!(
                    "{name} became a chat, whose agent did not restart: {error:#}; continue it with shoal restore {number}"
                )
            }
            Pending::Chat { .. } => format!(
                "{name} did not become a chat: {error:#}; it stays a workspace, whose stopped agent shoal restore {number} continues"
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

/// Continue the last session of the workspace `id` as `shoal restore` does,
/// where its agent ran at `origin` or in a background place.
async fn restore_workspace(
    paths: &crate::paths::Paths,
    id: &SpaceId,
    origin: Option<&Origin>,
) -> Result<()> {
    use anyhow::Context as _;
    use std::process::Stdio;

    #[cfg(any(test, feature = "test-support"))]
    if crate::daemon::fake_workers::faked() {
        fake::restore(paths, id);
        return Ok(());
    }

    let mut argv = crate::workers::shoal_argv(paths)?;
    argv.extend(["restore".into(), id.to_string().into()]);
    let env = crate::terminal::detached_start_environment(origin);
    let output = crate::workers::detached_command(&argv)
        .envs(&env)
        .current_dir(&paths.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("run shoal restore")?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
mod fake;
#[cfg(test)]
pub(super) mod tests;
