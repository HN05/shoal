//! Restarting a converted space's agent in the new place once its turn
//! ends, from a fork of its session, or handing the space's work over to
//! another agent tool.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::RunningAgent;
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use anyhow::Result;

use super::{
    Pending,
    handoff::{Leaving, Taking},
    pending::{Carrying, ConvertedChat},
    record::ChatRow,
};
use crate::protocol::operations::{Ended, Failure};
use crate::{
    agent::Agent,
    config::modes::ModeName,
    execution::recovery::{self, LastSession, LastSessionExt},
    sessions::Origin,
    state::WorkspaceState,
    {
        log,
        notifications::{About, AlertKind},
    },
};
use shoal_protocol::space_names::SpaceSelector;

/// From the hand-over until the agent's restart in the new place is queued,
/// or fails to be.
#[derive(Default)]
pub struct Relocating(std::sync::Mutex<std::collections::HashSet<SpaceId>>);

impl Relocating {
    /// Marked until the guard drops.
    pub(crate) fn mark(&self, id: &SpaceId) -> RelocatingMark<'_> {
        self.0.lock().unwrap().insert(id.clone());
        RelocatingMark(self, id.clone())
    }

    pub fn contains(&self, id: &SpaceId) -> bool {
        self.0.lock().unwrap().contains(id)
    }
}

pub(crate) struct RelocatingMark<'a>(&'a Relocating, SpaceId);

impl Drop for RelocatingMark<'_> {
    fn drop(&mut self) {
        (self.0).0.lock().unwrap().remove(&self.1);
    }
}

#[derive(Clone, Copy)]
pub struct ConversionStop {
    /// The stop keeps them down until `shoal resume`, which keeps the chat a
    /// conversion makes of the workspace stopped too.
    pub holds: bool,
    /// The workspace goes, which drops the switch to another agent tool its
    /// conversion asked for.
    pub removes: bool,
}

/// The mode its agent goes on in, the row the chat had, the execution its
/// agent runs or ran as, and how it carries on.
struct Converted {
    mode: ModeName,
    chat: ChatRow,
    execution: ExecutionId,
    carrying: Carrying,
}

impl Spaces {
    /// Each runs under an operation guard, so an update waits for it. The
    /// hand-overs stay held until the new agent is admitted, so a stop of
    /// the workspace's agents, which takes them too, comes either before,
    /// and the relocation reads the conversion as that stop left it, or
    /// after the new agent runs, which it stops.
    pub async fn relocate_finished_turns(&self, daemon: &dyn Daemon) -> Result<()> {
        let pending = self.store.run(|db| super::pending::all_pending(db)).await?;
        #[cfg(any(test, feature = "test-support"))]
        LISTED.notify_one();
        for id in pending.into_iter().map(|(id, _)| id) {
            let _operation = self.gates.operation().await;
            let handovers = &self.conversion_handovers;
            handovers.hold(self.relocate(daemon, &id)).await?;
        }
        Ok(())
    }

    /// Reads the conversion again under the hand-overs.
    async fn relocate(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<()> {
        let space = id.clone();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        let Some(pending) = pending else {
            return Ok(());
        };
        if !self.ready_to_relocate(daemon, id, &pending).await? {
            return Ok(());
        }
        // Read again once the turn ended, with the reports that change it,
        // its agent's sessions and its preparation's end, held off.
        let space = id.clone();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        let Some(pending) = pending else {
            return Ok(());
        };
        // Attaches wait for the agent's next session while this moves it.
        let _moving = (pending.restarts_agent()).then(|| self.relocating.mark(id));
        let relocated = match pending.clone() {
            Pending::Workspace {
                mode,
                chat,
                execution,
                hook_failure,
                carrying,
                ..
            } => {
                let converted = Converted {
                    mode,
                    chat,
                    execution,
                    carrying,
                };
                match hook_failure {
                    Some(failure) => {
                        (self.stop_after_failed_hook(daemon, id, converted, &failure)).await
                    }
                    None => self.relocate_to_workspace(daemon, id, converted).await,
                }
            }
            Pending::Chat {
                chat,
                held,
                carrying,
            } => (self.relocate_to_chat(daemon, id, *chat, held, carrying)).await,
            Pending::Agent {
                execution,
                taking,
                held,
            } => (self.switch_after_turn(daemon, id, &execution, *taking, held)).await,
        };
        if let Err(error) = relocated {
            self.report_failed_relocation(daemon, id, &pending, &error)
                .await;
        }
        // An agent kept down or not restarted no longer works its issues.
        daemon.settle_agents(id);
        Ok(())
    }

    /// The agent ended its turn, or its process its run, and the new place is
    /// ready: a new workspace once it is ready, after its setup or the
    /// repair of a setup that failed.
    async fn ready_to_relocate(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        pending: &Pending,
    ) -> Result<bool> {
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
                daemon
                    .turn_ended(id, &|agent: &RunningAgent| agent.space_id == *id)
                    .await
            }
            Pending::Chat { chat, .. } => {
                (daemon.turn_ended(id, &|agent: &RunningAgent| agent.ran_as(&chat.execution))).await
            }
            Pending::Agent { execution, .. } => {
                (daemon.turn_ended(id, &|agent: &RunningAgent| agent.ran_as(execution))).await
            }
        }
    }

    /// Post-setup hook included. One no longer followed ended long before.
    async fn preparation_over(&self, operation: Option<&str>) -> bool {
        let Some(operation) = operation else {
            return true;
        };
        self.operations.over(operation)
    }

    /// Kept beyond the operation's own record, which goes some time after it
    /// ends and with the daemon. A workspace without such a conversion keeps
    /// nothing.
    pub async fn keep_preparation_end(&self, id: &SpaceId, operation: &str, ended: &Ended) {
        let hook_failure = match ended {
            Ended::Failed(Failure::HookFailed { message }) => Some(message.clone()),
            _ => None,
        };
        let (space, operation) = (id.to_owned(), operation.to_owned());
        let kept = (self.store).run(move |db| {
            super::pending::record_prepared(db, &space, &operation, hook_failure.as_deref())
        });
        // Under the hand-overs, so no relocation acts on the end it misses.
        if let Err(error) = self.conversion_handovers.hold(kept).await {
            log!("preparation end of {id} not kept: {error:#}");
        }
    }

    /// Runs because the post-setup hook failed with `failure`. Stops as a
    /// stop of the workspace's agents does, keeping what carries on for
    /// `shoal resume`, and tells the user. A hand-over that fails leaves the
    /// conversion pending, to try again.
    async fn stop_after_failed_hook(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        converted: Converted,
        failure: &str,
    ) -> Result<()> {
        if let Err(error) = self.hand_over(daemon, id, converted).await {
            log!("agent of {id} not handed over yet: {error:#}");
            return Ok(());
        }
        let workspace = self.workspace_by_id(id).await?;
        let (name, number) = (workspace.name, workspace.number);
        let message = format!(
            "The post-setup hook of {name} failed: {failure}; its agent stopped instead of restarting there, and shoal resume {number} continues it once the hook is repaired"
        );
        let about = About::space(&name, id);
        (daemon
            .coordination()
            .notify_about(daemon, about, AlertKind::RecoveryBlocked, message))
        .await;
        Ok(())
    }

    /// Keeps the agent as [`Self::hand_over`] does. A hand-over that fails
    /// leaves the agent running where it was and the conversion pending, to
    /// try again.
    async fn relocate_to_workspace(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        converted: Converted,
    ) -> Result<()> {
        let origin = match self.hand_over(daemon, id, converted).await {
            Ok(origin) => origin,
            Err(error) => {
                log!("agent of {id} not handed over yet: {error:#}");
                return Ok(());
            }
        };
        restore_workspace(&self.paths, id, origin.as_ref(), self.operations.fakes()).await
    }

    /// As [`Self::hand_off`] does, once the agent `execution` runs or ran
    /// ended its turn. Keeps it stopped when `held`.
    pub(super) async fn switch_after_turn(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        execution: &ExecutionId,
        taking: Taking,
        held: bool,
    ) -> Result<()> {
        let leaving = self.leaving_agent(id, execution).await?;
        self.hand_off(daemon, id, leaving, taking, held).await
    }

    /// Runs with the hand-overs held. A stop that keeps the agents down holds
    /// a conversion into a chat. The agent of a chat that became the
    /// workspace, which the stop finds nowhere else, is handed over to it,
    /// once its session is kept as the workspace's last session, which
    /// `shoal resume` continues. Returns whether it had such an agent.
    pub async fn stop_converting_agent(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        stop: ConversionStop,
    ) -> Result<bool> {
        let handovers = &self.conversion_handovers;
        handovers.hold(self.settle_for_stop(daemon, id, stop)).await
    }

    /// Call once the hand-overs are held.
    async fn settle_for_stop(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        stop: ConversionStop,
    ) -> Result<bool> {
        if stop.holds {
            self.hold_converting_agent(id).await?;
        }
        let space = id.to_owned();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        let Some(Pending::Workspace {
            mode,
            chat,
            execution,
            carrying,
            ..
        }) = pending
        else {
            return Ok(false);
        };
        // A removal keeps no copies for another agent tool, which never
        // takes over there.
        let carrying = match stop.removes {
            true => Carrying::default(),
            false => carrying,
        };
        let converted = Converted {
            mode,
            chat,
            execution,
            carrying,
        };
        self.hand_over(daemon, id, converted).await?;
        Ok(true)
    }

    /// As [`Self::stop_converting_agent`] does, also within a relocation's
    /// removal of the spaces above it.
    pub async fn hand_over_before_removal(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<()> {
        let space = id.to_owned();
        let pending = self.store.run(move |db| super::pending(db, &space)).await?;
        if let Some(Pending::Workspace { .. }) = pending {
            let stop = ConversionStop {
                holds: false,
                removes: true,
            };
            self.stop_converting_agent(daemon, id, stop).await?;
        }
        Ok(())
    }

    /// Keeps the chat the workspace becomes, or the agent tool that takes
    /// its work over, stopped, as a stop keeps that agent down until `shoal
    /// resume`.
    async fn hold_converting_agent(&self, id: &SpaceId) -> Result<()> {
        let id = id.to_owned();
        (self.store)
            .run(move |db| super::pending::hold(db, &id))
            .await
    }

    /// Keeps what carries on first, then stops the agent where it ran and
    /// forgets the conversion and the scope the agent carried there. What
    /// carries on is the agent's session as the workspace's last session
    /// under its execution, marked converted, or else the agent tool that
    /// takes the work over from copies of its sessions. It is saved before
    /// the conversion goes, so a failure leaves it to be tried again.
    /// Returns where the agent ran in a terminal host, when it ran in one.
    async fn hand_over(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        converted: Converted,
    ) -> Result<Option<Origin>> {
        let Converted {
            mode,
            chat,
            execution,
            carrying,
        } = converted;
        let row = chat.agent()?;
        let leaving = self.leaving_agent(id, &execution).await?;
        let origin = leaving.as_ref().and_then(|leaving| leaving.origin.clone());
        match (carrying.taking, leaving) {
            (Some(taking), leaving) => {
                let listed = self.copies_for_taking(daemon, id).await?;
                (self.save_taking(daemon, id, leaving, *taking, &listed)).await?;
            }
            (None, Some(leaving)) => {
                let forked = forked(leaving, mode, carrying.model, row.narrowing()?);
                forked.write(&self.paths, id)?;
            }
            (None, None) => {}
        }
        // The workspace runs nothing else before the agent moves there.
        let request = crate::stops::StopRequest::conversion();
        (daemon.stop_space_agent_executions(id, &request)).await?;
        let (space, token) = (id.to_owned(), row.scope_token.clone());
        self.store
            .write(move |tx| super::record::relocated_chat(tx, &space, token.as_ref()))
            .await?;
        self.signals.agent_changed();
        Ok(origin)
    }

    async fn copies_for_taking(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Vec<super::handoff::Listed>> {
        let space = self.space(&SpaceSelector::id(id)).await?;
        self.copy_sessions(daemon, &space).await
    }

    /// With the session it continues.
    fn converted_agent(&self, id: &SpaceId, chat: &ConvertedChat) -> Leaving {
        Leaving {
            agent: chat.agent.clone(),
            session: self.latest_session(id, &chat.execution),
            execution: chat.execution.clone(),
            origin: chat.origin.clone(),
        }
    }

    /// The one that runs or ran as `execution`, with the session it
    /// continues, else the one the space saved last.
    async fn leaving_agent(
        &self,
        id: &SpaceId,
        execution: &ExecutionId,
    ) -> Result<Option<Leaving>> {
        let Some((agent, origin)) = self.converting_agent(id, execution).await? else {
            return crate::spaces::sessions::newest_saved(&self.paths, id);
        };
        Ok(Some(Leaving {
            agent,
            session: self.latest_session(id, execution),
            execution: execution.clone(),
            origin,
        }))
    }

    /// With where it ran in a terminal host: as its execution records it
    /// while it runs, else as its last session kept it when it exited. None
    /// once neither does.
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

    /// Stops the agent, removes the worktree as `shoal rm` does, and restores
    /// the chat's agent in the repository's checkout through the agent
    /// queue. The agent continues the session it reported last, whether it
    /// still runs or exited, told what the space is now, or else the agent
    /// tool that takes its work over starts from copies of its sessions. A
    /// stop that `held` it leaves the chat for `shoal resume`.
    async fn relocate_to_chat(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        chat: ConvertedChat,
        held: bool,
        carrying: Carrying,
    ) -> Result<()> {
        // What leaves is read before the removal stops it.
        let (leaving, listed) = match &carrying.taking {
            Some(_) => (
                self.leaving_agent(id, &chat.execution).await?,
                self.copies_for_taking(daemon, id).await?,
            ),
            None => (Some(self.converted_agent(id, &chat)), Vec::new()),
        };
        self.convert_worktree_to_chat(daemon, id, &chat).await?;
        match (carrying.taking, leaving) {
            (Some(taking), leaving) => {
                (self.save_taking(daemon, id, leaving, *taking, &listed)).await?;
            }
            (None, Some(leaving)) => {
                let forked = forked(leaving, chat.mode.clone(), carrying.model, chat.narrowing);
                forked.write(&self.paths, id)?;
            }
            (None, None) => {}
        }
        if held {
            return Ok(());
        }
        let env = crate::terminal::detached_start_environment(chat.origin.as_ref());
        let selector = shoal_protocol::space_names::SpaceSelector::id(id);
        (daemon
            .start_stopped_restore(&selector, &chat.execution, env)
            .await)
            .map(drop)
    }

    /// While the agent runs, the one it reported last, or else carried from
    /// the agent it restored. Otherwise as its last session kept it when it
    /// exited.
    fn latest_session(&self, id: &SpaceId, execution: &ExecutionId) -> Option<AgentSessionId> {
        let carried = recovery::carried::continued(&self.paths, id, execution);
        carried.session.or_else(|| {
            let path = LastSession::path(&self.paths, id, execution);
            LastSession::load(&path).ok().flatten()?.session
        })
    }

    /// Forgets the conversion, which is not tried again.
    async fn report_failed_relocation(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        pending: &Pending,
        error: &anyhow::Error,
    ) {
        log!("space {id} not relocated: {error:#}");
        let space = id.to_owned();
        if let Err(error) = self
            .store
            .run(move |db| super::pending::forget(db, &space))
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
                "The agent of {name} did not restart in its new worktree: {error:#}; continue it with shoal resume {number}"
            ),
            Pending::Chat { .. } if self.chat_by_id(id).await.is_ok_and(|chat| chat.is_some()) => {
                format!(
                    "{name} became a chat, whose agent did not restart: {error:#}; continue it with shoal resume {number}"
                )
            }
            Pending::Chat { .. } => format!(
                "{name} did not become a chat: {error:#}; it stays a workspace, whose stopped agent shoal resume {number} continues"
            ),
            Pending::Agent { taking, .. } => format!(
                "The work of {name} did not go over to {}: {error:#}; continue it with shoal resume {number}, or convert it again",
                taking.agent
            ),
        };
        daemon
            .coordination()
            .notify_about(
                daemon,
                About::space(&name, id),
                AlertKind::RecoveryBlocked,
                message,
            )
            .await;
    }
}

/// `leaving` continues from a fork of its session in the space it
/// converted, in `mode` with `model` and the permissions `narrowing` gives
/// it.
fn forked(
    leaving: Leaving,
    mode: ModeName,
    model: crate::agent::ModelChoice,
    narrowing: crate::config::permissions::PermissionsConfig,
) -> LastSession {
    LastSession {
        execution: leaving.execution,
        agent: leaving.agent,
        session: leaving.session,
        moved: false,
        copied: None,
        converted: true,
        handoff: None,
        origin: leaving.origin,
        stop: None,
        turn: None,
        model,
        mode,
        narrowing,
    }
}

/// As `shoal resume` does, where the agent ran at `origin` or in a
/// background place.
async fn restore_workspace(
    paths: &crate::paths::Paths,
    id: &SpaceId,
    origin: Option<&Origin>,
    fake: bool,
) -> Result<()> {
    use std::process::Stdio;

    #[cfg(any(test, feature = "test-support"))]
    if fake {
        fake::restore(paths, id);
        return Ok(());
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = fake;

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
        .await;
    shoal_subprocess::checked_output("shoal resume", output)?;
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
pub mod fake;

/// Notified each time the relocation pass lists the pending conversions,
/// which tests wait for.
#[cfg(any(test, feature = "test-support"))]
pub static LISTED: tokio::sync::Notify = tokio::sync::Notify::const_new();
