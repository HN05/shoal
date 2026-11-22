//! `shoal restart`: an agent stops as after a protection stop, and its session
//! starts again at once and outside the agent queue, since the agent keeps its
//! slot: in a new wrapper, which the agent queue starts once the old one
//! finished, or in place, where the old wrapper starts it.
use crate::daemon::agents::ProtectionExt;
use anyhow::{Context, Result, ensure};
use shoal_ids::execution_ids::ExecutionId;
use tokio::sync::oneshot;

use super::{Agent, Manager, stops};
use crate::daemon::log;
use crate::daemon::turn_state;
use crate::{
    config::permissions::PermissionChange,
    daemon::store,
    execution::recovery,
    model::{Protection, RunningAgent},
    sessions::Origin,
    state::AgentState,
};

mod renewal;
mod state;
use renewal::Renewal;
pub(crate) use renewal::{Renewals, forget_ended as forget_ended_renewals};
pub(crate) use state::{Outcome, Restart, Restarts, restarted};

impl Manager {
    /// Every connected agent, every execution whose wrapper is gone, which
    /// may have run one, every agent whose wrapper reattaches before this
    /// daemon tracks it again, and every agent a restart starts in a new
    /// wrapper, by the execution it ran as; connected commands are left out.
    pub async fn running_agents(&self) -> Result<Vec<RunningAgent>> {
        // Before the records, so an agent tracked meanwhile shows with what
        // its tracking left recorded, such as a stop a reattach forgot.
        let connections = self.executions.connections().await;
        let (executions, reported, stops) = (self.store)
            .read(|db| {
                let executions = store::all_executions(db)?;
                Ok((executions, turn_state::reported_turns(db)?, stops::all(db)?))
            })
            .await?;
        Ok(connections
            .with_restarts(|agents, renewals| {
                let mut listed: Vec<RunningAgent> = executions
                    .into_iter()
                    // Listed below as the restart left it.
                    .filter(|execution| !renewal::starting(renewals, &execution.id))
                    .filter_map(|execution| {
                        let agent = agents.get(&execution.id);
                        // Its record names the agent of a reattaching wrapper.
                        let command = agent.is_none() && execution.agent.is_none();
                        if command && connections.is_connected(&execution.id) {
                            return None;
                        }
                        Some(RunningAgent {
                            // Named as `--agent` and recovery records name it.
                            agent: agent.map(|agent| agent.name.normalized()),
                            restorable: agent.is_some_and(|agent| agent.recover),
                            state: agent.and(reported.get(&execution.id).map(|turn| turn.0)),
                            interrupted: agent.is_some()
                                && reported.get(&execution.id).is_some_and(|turn| turn.1),
                            stopped: agent.and(stops.get(&execution.id).cloned()),
                            restarted_from: renewal::earlier(renewals, &execution.id),
                            execution: execution.id,
                            space_id: execution.space_id,
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
    /// it when that runs in a terminal host. The agent starts again with
    /// `change` made to the permissions it was given.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn restart_agent(
        &self,
        execution: &ExecutionId,
        in_place: bool,
        change: Option<PermissionChange>,
    ) -> Result<Option<Origin>> {
        let answer = (self.begin_restart(execution, in_place, change)).await?;
        restarted(answer.answers()?).await
    }

    /// Ask the agent's wrapper to stop it for a restart, or join the restart
    /// under way, unless that settled how it starts the agent already. `id`
    /// may name an execution the agent ran as before a restart started it
    /// in a new wrapper.
    pub(crate) async fn begin_restart(
        &self,
        id: &ExecutionId,
        in_place: bool,
        change: Option<PermissionChange>,
    ) -> Result<Requested> {
        let current = (self.executions)
            .with_restarts(|_, renewals| renewal::current(renewals, id))
            .await;
        let stopped = self.agent_overload(&current).await;
        let turn = self.agents.reported_turn(&current).await;
        self.executions
            .with_restarts(|agents, renewals| {
                let id = renewal::current(renewals, id);
                if let Some(Renewal::Starting(_)) = renewals.get(&id) {
                    return Ok(Requested::Settled);
                }
                // A stop of an agent that runs again meanwhile shows only on
                // its stop channel, and its turn is unknown.
                let (stopped, turn) = match id == current {
                    true => (stopped, turn),
                    false => (None, None),
                };
                let agent = agents.get_mut(&id).map(|agent| (agent, stopped, turn));
                self.stop_for_restart(&id, agent, in_place, change)
            })
            .await
    }

    /// Save the handoff of `agent`, the agent of `id` with the protection
    /// stop that stopped it if any and the turn state it reported, then stop
    /// it for a restart, `in_place` or in a new wrapper, starting it with
    /// `change` made when given, or join the restart under way, which then
    /// does so, unless it settled already.
    fn stop_for_restart(
        &self,
        id: &ExecutionId,
        agent: Option<(&mut Agent, Option<Protection>, Option<AgentState>)>,
        in_place: bool,
        change: Option<PermissionChange>,
    ) -> Result<Requested> {
        let (agent, stopped, turn) =
            agent.with_context(|| format!("no connected agent runs execution {id}"))?;
        if let Some(restart) = &mut agent.restarts.current {
            return Ok(match restart.merge(in_place, change) {
                true => Requested::Answers(restart.answer()),
                false => Requested::Settled,
            });
        }
        ensure!(
            agent.recover,
            "{} has no command to resume its session; configure its [agents.<name>] resume_command",
            agent.name
        );
        agent.ensure_not_stopped(stopped.as_ref())?;
        // Save the handoff before the stop, so a wrapper that dies stopping
        // the agent leaves it for `shoal resume`.
        let handoff = recovery::save_record(
            &self.paths,
            &agent.workspace_id,
            id,
            &agent.tracked(),
            Some(&Protection::restart().reason),
            turn,
        )
        .context("save the agent's recovery record")?;
        let mut restart = Restart::new(in_place, change);
        let answer = restart.answer();
        agent.restarts.current = Some(restart);
        if agent.stop.send(true).is_err() {
            agent.restarts.current = None;
            recovery::consume(&handoff)?;
            anyhow::bail!("{} disconnected", agent.name);
        }
        Ok(Requested::Answers(answer))
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

    /// Forget the restart the daemon recorded and began for the agent of
    /// `id`, whose restore applies its options now.
    pub(crate) async fn forget_applied_restart(&self, id: &ExecutionId) -> Result<()> {
        let execution = id.to_owned();
        (self.store)
            .write(move |tx| crate::daemon::restarts::record::applied(tx, &execution))
            .await
    }

    /// Let the restart the daemon recorded and began for the agent of `id`
    /// begin again, as its wrapper reattached still running the agent,
    /// without the stop the previous daemon sent.
    pub(crate) async fn restart_undelivered(&self, id: &ExecutionId) {
        let execution = id.to_owned();
        let reset = (self.store)
            .write(move |tx| crate::daemon::restarts::record::undelivered(tx, &execution))
            .await;
        if let Err(error) = reset {
            log!("undelivered restart of execution {id} not reset: {error:#}");
        }
    }

    /// Whether something stops the tracked agent of `id`, such as a restart
    /// or `shoal stop`, which a turn that starts now would not outlast.
    pub(crate) async fn agent_stopping(&self, id: &ExecutionId) -> bool {
        (self.executions.with_agent(id, |agent| !agent.running()))
            .await
            .unwrap_or(false)
    }

    /// Whether `shoal restart` is restarting the agent of `id`.
    pub(crate) async fn restart_requested(&self, id: &ExecutionId) -> bool {
        self.executions
            .with_agent(id, |agent| agent.restarts.current.is_some())
            .await
            .unwrap_or(false)
    }

    /// Settle how the restart of `id` starts the agent again, which a newer
    /// request no longer changes, returning whether in its wrapper.
    pub(crate) async fn settle_restart(&self, id: &ExecutionId) -> bool {
        self.executions
            .with_agent(id, |agent| {
                agent.restarts.current.as_mut().is_some_and(Restart::settle)
            })
            .await
            .unwrap_or(false)
    }

    /// What the restart under way of the agent of `id` changes in the
    /// permissions it was given.
    #[cfg(test)]
    pub(crate) async fn restart_change(&self, id: &ExecutionId) -> Option<PermissionChange> {
        (self.executions)
            .with_agent(id, |agent| {
                agent.restarts.current.as_ref()?.change().cloned()
            })
            .await
            .flatten()
    }

    /// Whether a restart of the agent of `id` is under way, in its wrapper
    /// or for a new one.
    pub(crate) async fn restarting(&self, id: &ExecutionId) -> bool {
        self.restart_requested(id).await || self.restart_continues_elsewhere(id).await
    }

    /// Leave the restart of `id`, whose agent's processes stopped, to a new
    /// wrapper, which starts the agent once this one finished, with the
    /// change the restart makes to the permissions it was given, which its
    /// wrapper's record takes now. The restart is kept apart from the agent
    /// from now on, while the execution is still recorded, so something
    /// always shows the agent to a stop.
    pub(crate) async fn hand_over_restart(&self, id: &ExecutionId) -> Result<()> {
        self.executions
            .with_restarts(|agents, renewals| {
                let agent = agents.get_mut(id).context("the restart was cancelled")?;
                self.save_restart_change(id, agent)?;
                let renewal = agent.hand_over().context("the restart was cancelled")?;
                renewals.insert(id.to_owned(), renewal);
                Ok(())
            })
            .await
    }

    /// Give the record of `id`, whose agent restarts in its wrapper, the
    /// change the restart makes to the permissions it was given, which
    /// that wrapper restores it under.
    pub(crate) async fn give_restart_change(&self, id: &ExecutionId) -> Result<()> {
        (self.executions)
            .with_agent(id, |agent| self.save_restart_change(id, agent))
            .await
            .context("the restart was cancelled")?
    }

    /// Make the change the restart of `agent`, the agent of `id`, makes to
    /// the permissions it was given in the record it restores from.
    fn save_restart_change(&self, id: &ExecutionId, agent: &Agent) -> Result<()> {
        let restart = agent.restarts.current.as_ref();
        if let Some(change) = restart.and_then(Restart::change) {
            recovery::change_permission(&self.paths, &agent.workspace_id, id, change)
                .context("give the restarted agent its permissions")?;
        }
        Ok(())
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
    /// leaves the place out of what reads it, such as a restart's report.
    pub(in crate::daemon) async fn execution_place(&self, id: &ExecutionId) -> Option<Origin> {
        let execution = id.to_owned();
        match self
            .store
            .run(move |db| store::find_execution(db, &execution))
            .await
        {
            Ok(execution) => execution.and_then(|execution| execution.origin),
            Err(error) => {
                log!("place of execution {id} not read: {error:#}");
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
        let answers = self
            .executions
            .with_agent(id, |agent| {
                let restart = agent.restarts.current.as_mut()?;
                restart.waits = Some(issue.to_owned());
                Some(std::mem::take(&mut restart.answers))
            })
            .await
            .flatten();
        for done in answers.into_iter().flatten() {
            // The request may have gone with its client.
            let _ = done.send(Err(RestartWaits(issue.to_owned()).into()));
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

/// What a request for an agent's restart began.
pub(crate) enum Requested {
    /// The agent restarts, alone or with the restart under way, answering
    /// once it runs again, which [`restarted`] reads.
    Answers(oneshot::Receiver<Result<Option<Origin>>>),
    /// The restart under way settled how it starts the agent already, which
    /// runs again before another restart can begin.
    Settled,
}

impl Requested {
    /// What answers once the agent runs again, refusing a restart that has
    /// to wait for the one under way.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn answers(self) -> Result<oneshot::Receiver<Result<Option<Origin>>>> {
        match self {
            Self::Answers(answer) => Ok(answer),
            Self::Settled => anyhow::bail!("the restart under way started the agent already"),
        }
    }
}

/// A restart that continues after its request was answered, once the
/// stopped agent's remaining processes exit, which the issue it holds keeps
/// from being proven.
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
