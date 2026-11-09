//! `shoal restart`: an agent stops as after a protection stop, and its session
//! starts again at once and outside the agent queue, since the agent keeps its
//! slot: in a new wrapper, which the agent queue starts once the old one
//! finished, or in place, where the old wrapper starts it.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use crate::daemon::workspace::agents::ProtectionExt;
use anyhow::{Context, Result, anyhow, ensure};
use tokio::sync::oneshot;

use super::{Agent, Manager};
use crate::daemon::log;
use crate::{
    daemon::store, execution::recovery, model::RunningAgent, protocol::Protection,
    sessions::Origin, state::AgentState,
};

mod renewal;
mod state;
use renewal::Renewal;
pub(crate) use renewal::{Renewals, forget_ended as forget_ended_renewals};
pub(crate) use state::{Outcome, Restart, Restarts, restarted};

impl Manager {
    /// Every connected agent, every execution whose wrapper is gone, which
    /// may have run one, and every agent a restart starts in a new wrapper,
    /// by the execution it ran as; connected commands are left out.
    pub(crate) async fn running_agents(&self) -> Result<Vec<RunningAgent>> {
        let executions = self.store.run(|db| store::all_executions(db)).await?;
        let connections = self.executions.connections().await;
        Ok(connections
            .with_restarts(|agents, renewals| {
                let mut listed: Vec<RunningAgent> = executions
                    .into_iter()
                    // Listed below as the restart left it.
                    .filter(|execution| !renewal::starting(renewals, &execution.id))
                    .filter_map(|execution| {
                        let agent = agents.get(&execution.id);
                        if agent.is_none() && connections.is_connected(&execution.id) {
                            return None;
                        }
                        Some(RunningAgent {
                            // Named as `--agent` and recovery records name it.
                            agent: agent.map(|agent| agent.name.replace(' ', "-")),
                            restorable: agent.is_some_and(|agent| agent.recover),
                            state: agent.and_then(|agent| agent.turn),
                            interrupted: agent.is_some_and(|agent| agent.interrupted),
                            restarted_by: agent
                                .map(|agent| agent.restarts.restarted_by())
                                .unwrap_or_default(),
                            restart: agent.and_then(|agent| agent.restarts.pending()),
                            stopped: agent.and_then(|agent| agent.overload.clone()),
                            restarted_from: renewal::earlier(renewals, &execution.id),
                            execution: execution.id,
                            space_id: execution.workspace_id,
                            origin: execution.origin,
                        })
                    })
                    .collect();
                listed.extend(renewal::handed_over(renewals));
                listed
            })
            .await)
    }

    /// Restart the agent of `execution`, `in_place` or in a new wrapper,
    /// returning once it runs again, with the place of the wrapper that runs
    /// it when that runs in a terminal host. `run` names the `shoal restart`
    /// run that asks for it, which the process of the space `stopped_by`
    /// runs when given.
    pub(crate) async fn restart_agent(
        &self,
        execution: &ExecutionId,
        run: Option<String>,
        in_place: bool,
        stopped_by: Option<&SpaceId>,
    ) -> Result<Option<Origin>> {
        let answer = self
            .request_restart(execution, run, in_place, stopped_by)
            .await?;
        restarted(answer).await
    }

    /// Wait for the restart of `execution` that `of_run` asked for as
    /// [`Self::restart_agent`] does, or answer as it did if it ended, even
    /// when another run restarts the agent now; it restarts the agent for
    /// `run` too. A restart whose run a daemon before this one forgot
    /// matches whichever is in progress.
    pub(crate) async fn join_restart(
        &self,
        execution: &ExecutionId,
        of_run: Option<String>,
        run: Option<String>,
    ) -> Result<Option<Origin>> {
        let joined = self
            .executions
            .with_restarts(|agents, renewals| {
                let id = renewal::current(renewals, execution);
                let (name, restarts) = match renewals.get_mut(&id) {
                    Some(Renewal::Starting(handover)) => (&handover.name, &mut handover.restarts),
                    _ => {
                        let agent = agents.get_mut(&id).with_context(|| {
                            format!("no connected agent runs execution {execution}")
                        })?;
                        (&agent.name, &mut agent.restarts)
                    }
                };
                anyhow::Ok(restarts.join(name, &of_run, run))
            })
            .await?;
        match joined {
            Ok(answer) => restarted(answer).await,
            Err(ended) => ended.map_err(|reason| anyhow!(reason)),
        }
    }

    /// Ask the agent's wrapper to stop it for a restart, or join the
    /// restart that starts it in a new wrapper. `id` may name an execution
    /// the agent ran as before such a restart.
    async fn request_restart(
        &self,
        id: &ExecutionId,
        run: Option<String>,
        in_place: bool,
        stopped_by: Option<&SpaceId>,
    ) -> Result<oneshot::Receiver<Result<Option<Origin>>>> {
        self.executions
            .with_restarts(|agents, renewals| {
                let id = renewal::current(renewals, id);
                if let Some(Renewal::Starting(handover)) = renewals.get_mut(&id) {
                    return Ok(handover.answer(run));
                }
                let agent = agents.get_mut(&id);
                self.stop_for_restart(&id, agent, run, in_place, stopped_by)
            })
            .await
    }

    /// Save the handoff of `agent`, the agent of `id`, then stop it for a
    /// restart, `in_place` or in a new wrapper, or join the restart already
    /// in progress as it was asked. The handoff names the space
    /// `stopped_by`, whose process asked for the restart, so it may resume
    /// an agent the restart leaves stopped.
    fn stop_for_restart(
        &self,
        id: &ExecutionId,
        agent: Option<&mut Agent>,
        run: Option<String>,
        in_place: bool,
        stopped_by: Option<&SpaceId>,
    ) -> Result<oneshot::Receiver<Result<Option<Origin>>>> {
        let agent = agent.with_context(|| format!("no connected agent runs execution {id}"))?;
        // Another command continuing the same run restarted it already,
        // even if another run restarts it since.
        if let Some(place) = agent.restarts.restarted_for(&run) {
            let (done, answer) = oneshot::channel();
            let _ = done.send(Ok(place));
            return Ok(answer);
        }
        if let Some(restart) = &mut agent.restarts.current {
            return Ok(restart.answer(run));
        }
        ensure!(
            agent.recover,
            "{} has no command to resume its session; configure its [agents.<name>] resume_command",
            agent.name
        );
        agent.ensure_not_stopped()?;
        // Save the handoff before the stop, so a wrapper that dies stopping
        // the agent leaves it for `shoal resume`.
        let handoff = recovery::save_record_stopped_by(
            &self.paths,
            &agent.workspace_id,
            id,
            &agent.tracked(),
            Some(&Protection::restart().reason),
            agent.turn,
            stopped_by,
        )
        .context("save the agent's recovery record")?;
        let mut restart = Restart::unanswered(run, in_place);
        let answer = restart.answer(None);
        agent.restarts.current = Some(restart);
        if agent.stop.send(true).is_err() {
            agent.restarts.current = None;
            recovery::consume(&handoff)?;
            anyhow::bail!("{} disconnected", agent.name);
        }
        Ok(answer)
    }

    /// Whether a restart of `id` has not resumed its agent yet; after the
    /// resume, a record left behind belongs to the wrapper that restores it.
    pub(crate) async fn restart_before_resume(&self, id: &ExecutionId) -> bool {
        self.executions
            .with_agent(id, |agent| {
                agent
                    .restarts
                    .current
                    .as_ref()
                    .is_some_and(|restart| !restart.resumed)
            })
            .await
            .unwrap_or(false)
    }

    /// Keep the turn state the agent of `id` reported.
    pub(crate) async fn record_agent_turn(
        &self,
        id: &ExecutionId,
        state: AgentState,
        interrupted: Option<bool>,
    ) {
        self.executions
            .with_agent(id, |agent| {
                if state != AgentState::Idle {
                    agent.turns_started += 1;
                }
                agent.turn = Some(state);
                agent.interrupted = interrupted.unwrap_or(agent.interrupted);
            })
            .await;
    }

    /// Mark the agent of `id` working on the turn a wake starts, unless a
    /// stop of it came first, under the lock a stop takes, so a stop that
    /// read the agent idle before cannot follow. Whether it is marked; an
    /// execution that runs no tracked agent has nothing to stop.
    pub(crate) async fn claim_woken_turn(&self, id: &ExecutionId) -> bool {
        self.executions
            .with_agent(id, |agent| {
                let running = agent.running();
                if running {
                    agent.turn = Some(AgentState::Working);
                }
                running
            })
            .await
            .unwrap_or(true)
    }

    /// Whether `shoal restart` is restarting the agent of `id`.
    pub(crate) async fn restart_requested(&self, id: &ExecutionId) -> bool {
        self.executions
            .with_agent(id, |agent| agent.restarts.current.is_some())
            .await
            .unwrap_or(false)
    }

    /// Whether the restart of `id` was asked to start the agent again in its
    /// wrapper.
    pub(crate) async fn restart_in_place(&self, id: &ExecutionId) -> bool {
        self.executions
            .with_agent(id, |agent| {
                agent
                    .restarts
                    .current
                    .as_ref()
                    .is_some_and(Restart::in_place)
            })
            .await
            .unwrap_or(false)
    }

    /// Leave the restart of `id`, whose agent's processes stopped, to a new
    /// wrapper, which starts the agent once this one finished. The restart
    /// is kept apart from the agent from now on, while the execution is
    /// still recorded, so something always shows the agent to a stop.
    pub(crate) async fn hand_over_restart(&self, id: &ExecutionId) -> Result<()> {
        self.executions
            .with_restarts(|agents, renewals| {
                let renewal = agents.get_mut(id).and_then(Agent::hand_over);
                let renewal = renewal.context("the restart was cancelled")?;
                renewals.insert(id.to_owned(), renewal);
                Ok(())
            })
            .await
    }

    /// Whether a new wrapper starts the agent of `id` again for its restart.
    pub(crate) async fn restart_continues_elsewhere(&self, id: &ExecutionId) -> bool {
        self.executions
            .with_restarts(|_, renewals| renewal::starting(renewals, id))
            .await
    }

    /// The wrapper of `id` started its child: once a restart resumed the
    /// agent, that child is the restarted agent, which runs in the
    /// wrapper's place.
    pub(crate) async fn agent_started(&self, id: &ExecutionId) {
        if !self.restart_resumed(id).await {
            return;
        }
        let place = self.execution_place(id).await;
        self.answer_restart(id, Restart::resumed, Ok(place)).await;
    }

    /// Whether a restart of `id` resumed its agent.
    async fn restart_resumed(&self, id: &ExecutionId) -> bool {
        self.executions
            .with_agent(id, |agent| {
                agent
                    .restarts
                    .current
                    .as_ref()
                    .is_some_and(Restart::resumed)
            })
            .await
            .unwrap_or(false)
    }

    /// The place the wrapper of `id` registered in a terminal host; none
    /// when it runs outside one or its record cannot be read, which only
    /// leaves the place out of the restart's report.
    async fn execution_place(&self, id: &ExecutionId) -> Option<Origin> {
        let id = id.to_owned();
        match self
            .store
            .run(move |db| store::find_execution(db, &id))
            .await
        {
            Ok(execution) => execution.and_then(|execution| execution.origin),
            Err(error) => {
                log!("place of restarted execution not read: {error:#}");
                None
            }
        }
    }

    /// The restart of `id` ends without the agent running again.
    pub(crate) async fn fail_restart(&self, id: &ExecutionId, reason: String) {
        self.answer_restart(id, |_| true, Err(reason)).await;
    }

    /// The restart of `id` waits for the stopped agent's processes to exit
    /// because of `issue`, which may last until the user ends them. Its
    /// requests are answered with that now, and later ones at once, while the
    /// restart continues.
    pub(crate) async fn restart_waits(&self, id: &ExecutionId, issue: &str) {
        let answered = self
            .executions
            .with_agent(id, |agent| {
                let restart = agent.restarts.current.as_mut()?;
                let waits = format!(
                    "{} restarts once its remaining processes exit: {issue}; shoal stop {} cancels the restart",
                    agent.name, agent.workspace
                );
                restart.waits = Some(waits.clone());
                Some((std::mem::take(&mut restart.answers), waits))
            })
            .await
            .flatten();
        if let Some((answers, waits)) = answered {
            for done in answers {
                // The request may have gone with its client.
                let _ = done.send(Err(RestartWaits(waits.clone()).into()));
            }
        }
    }

    /// Answer the restart of `id` with `outcome` if `ends` says it ended.
    async fn answer_restart(
        &self,
        id: &ExecutionId,
        ends: impl FnOnce(&Restart) -> bool,
        outcome: Outcome,
    ) {
        self.executions
            .with_agent(id, |agent| agent.restarts.finish_if(ends, &outcome))
            .await;
    }
}

/// A restart that continues after its request was answered, once the
/// stopped agent's remaining processes exit.
#[derive(Debug)]
pub struct RestartWaits(pub String);

impl std::fmt::Display for RestartWaits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RestartWaits {}

#[cfg(test)]
mod tests;
