//! `shoal restart`: an agent stops and its wrapper starts its session again
//! in the same terminal, as after a protection stop, but at once and outside
//! the agent queue, since the agent keeps its slot.
use anyhow::{Context, Result, anyhow, ensure};
use tokio::sync::oneshot;

use super::{Agent, Manager};
use crate::{
    daemon::store,
    execution::recovery,
    model::{PendingRestart, RunningAgent},
    protocol::Protection,
    state::AgentState,
};

/// A restart in progress, which answers each request for it.
pub(super) struct Restart {
    /// The requests still to answer; a request for an agent already
    /// restarting joins them.
    answers: Vec<oneshot::Sender<Result<()>>>,
    /// The `shoal restart` run that asked for it.
    run: Option<String>,
    /// The other runs whose requests joined it, for which it restarts the
    /// agent as well.
    joined: Vec<String>,
    /// The stopped agent was resumed, so its wrapper's next start is the
    /// restarted agent rather than the first one.
    resumed: bool,
    /// Why the restart waits for the stopped agent's processes, which
    /// answers each request at once.
    waits: Option<String>,
}

impl Restart {
    /// A restart for `run` without requests to answer, such as one whose
    /// request ended with the daemon that took it.
    pub(super) fn unanswered(run: Option<String>) -> Self {
        Self {
            answers: Vec::new(),
            run,
            joined: Vec::new(),
            resumed: false,
            waits: None,
        }
    }

    /// Answer one more request, of `run`: at once while the restart waits
    /// for the agent's processes, otherwise once it ends.
    fn answer(&mut self, run: Option<String>) -> oneshot::Receiver<Result<()>> {
        if let Some(run) = run
            && self.run.as_ref() != Some(&run)
            && !self.joined.contains(&run)
        {
            self.joined.push(run);
        }
        let (done, answer) = oneshot::channel();
        match &self.waits {
            Some(waits) => {
                let _ = done.send(Err(RestartWaits(waits.clone()).into()));
            }
            None => self.answers.push(done),
        }
        answer
    }

    pub(super) fn resume(&mut self) {
        self.resumed = true;
        self.waits = None;
    }

    fn pending(&self) -> PendingRestart {
        PendingRestart {
            run: self.run.clone(),
        }
    }

    /// Every run it restarts the agent for.
    fn runs(&self) -> impl Iterator<Item = Option<String>> + '_ {
        std::iter::once(self.run.clone()).chain(self.joined.iter().cloned().map(Some))
    }
}

impl Manager {
    /// Every connected agent, and every execution whose wrapper is gone,
    /// which may have run one; connected commands are left out.
    pub(crate) async fn running_agents(&self) -> Result<Vec<RunningAgent>> {
        let executions = self.store.run(|db| store::all_executions(db)).await?;
        let connections = self.executions.connections().await;
        Ok(connections
            .with_agents(|agents| {
                executions
                    .into_iter()
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
                            restarted_by: agent.map(Agent::restarted_by).unwrap_or_default(),
                            restart: agent
                                .and_then(|agent| agent.restart.as_ref())
                                .map(Restart::pending),
                            execution: execution.id,
                            workspace_id: execution.workspace_id,
                            origin: execution.origin,
                        })
                    })
                    .collect()
            })
            .await)
    }

    /// Restart the agent of `execution`, returning once it runs again.
    /// `run` names the `shoal restart` run that asks for it.
    pub(crate) async fn restart_agent(&self, execution: &str, run: Option<String>) -> Result<()> {
        restarted(self.request_restart(execution, run).await?).await
    }

    /// Wait for the restart of `execution` that `of_run` asked for as
    /// [`Self::restart_agent`] does, or answer as it did if it ended, even
    /// when another run restarts the agent now; it restarts the agent for
    /// `run` too. A restart whose run a daemon before this one forgot
    /// matches whichever is in progress.
    pub(crate) async fn join_restart(
        &self,
        execution: &str,
        of_run: Option<String>,
        run: Option<String>,
    ) -> Result<()> {
        let joined = self
            .executions
            .with_agents(|agents| {
                let agent = agents
                    .get_mut(execution)
                    .with_context(|| format!("no connected agent runs execution {execution}"))?;
                anyhow::Ok(match &mut agent.restart {
                    Some(restart) if of_run.is_none() || restart.run == of_run => {
                        Ok(restart.answer(run))
                    }
                    _ => Err(agent.joined_late(&of_run, run)),
                })
            })
            .await?;
        match joined {
            Ok(answer) => restarted(answer).await,
            Err(ended) => ended.map_err(|reason| anyhow!(reason)),
        }
    }

    /// Ask the agent's wrapper to stop it for a restart.
    async fn request_restart(
        &self,
        id: &str,
        run: Option<String>,
    ) -> Result<oneshot::Receiver<Result<()>>> {
        self.executions
            .with_agents(|agents| self.stop_for_restart(id, agents.get_mut(id), run))
            .await
    }

    /// Save the handoff of `agent`, the agent of `id`, then stop it for a
    /// restart, or join the restart already in progress.
    fn stop_for_restart(
        &self,
        id: &str,
        agent: Option<&mut Agent>,
        run: Option<String>,
    ) -> Result<oneshot::Receiver<Result<()>>> {
        let agent = agent.with_context(|| format!("no connected agent runs execution {id}"))?;
        // Another command continuing the same run restarted it already,
        // even if another run restarts it since.
        if run.is_some() && agent.ended_restarts.get(&run) == Some(&Ok(())) {
            let (done, answer) = oneshot::channel();
            let _ = done.send(Ok(()));
            return Ok(answer);
        }
        if let Some(restart) = &mut agent.restart {
            return Ok(restart.answer(run));
        }
        ensure!(
            agent.recover,
            "{} has no command to resume its session; configure [agent_resume] for it",
            agent.name
        );
        ensure!(!*agent.stop.borrow(), "{} is already stopping", agent.name);
        // Save the handoff before the stop, so a wrapper that dies stopping
        // the agent leaves it for `shoal resume`.
        let handoff = recovery::save_record(
            &self.paths,
            &agent.workspace_id,
            id,
            &agent.name,
            Some(&Protection::restart().reason),
            agent.turn,
        )
        .context("save the agent's recovery record")?;
        let mut restart = Restart::unanswered(run);
        let answer = restart.answer(None);
        agent.restart = Some(restart);
        if agent.stop.send(true).is_err() {
            agent.restart = None;
            recovery::consume(&handoff)?;
            anyhow::bail!("{} disconnected", agent.name);
        }
        Ok(answer)
    }

    /// Whether a restart of `id` has not resumed its agent yet; after the
    /// resume, a record left behind belongs to the wrapper.
    pub(crate) async fn restart_before_resume(&self, id: &str) -> bool {
        self.executions
            .with_agent(id, |agent| {
                agent
                    .restart
                    .as_ref()
                    .is_some_and(|restart| !restart.resumed)
            })
            .await
            .unwrap_or(false)
    }

    /// Keep the turn state the agent of `id` reported.
    pub(crate) async fn record_agent_turn(
        &self,
        id: &str,
        state: AgentState,
        interrupted: Option<bool>,
    ) {
        self.executions
            .with_agent(id, |agent| {
                agent.turn = Some(state);
                agent.interrupted = interrupted.unwrap_or(agent.interrupted);
            })
            .await;
    }

    /// Whether `shoal restart` is restarting the agent of `id`.
    pub(crate) async fn restart_requested(&self, id: &str) -> bool {
        self.executions
            .with_agent(id, |agent| agent.restart.is_some())
            .await
            .unwrap_or(false)
    }

    /// The wrapper of `id` started its child: once a restart resumed the
    /// agent, that child is the restarted agent.
    pub(crate) async fn agent_started(&self, id: &str) {
        self.answer_restart(id, true, Ok(())).await;
    }

    /// The restart of `id` ends without the agent running again.
    pub(crate) async fn fail_restart(&self, id: &str, reason: String) {
        self.answer_restart(id, false, Err(reason)).await;
    }

    /// The restart of `id` waits for the stopped agent's processes to exit
    /// because of `issue`, which may last until the user ends them. Its
    /// requests are answered with that now, and later ones at once, while the
    /// restart continues.
    pub(crate) async fn restart_waits(&self, id: &str, issue: &str) {
        let answered = self
            .executions
            .with_agent(id, |agent| {
                let restart = agent.restart.as_mut()?;
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

    async fn answer_restart(&self, id: &str, once_resumed: bool, outcome: Result<(), String>) {
        let restart = self
            .executions
            .with_agent(id, |agent| {
                let restart = agent
                    .restart
                    .take_if(|restart| restart.resumed || !once_resumed)?;
                agent.end_restart(&restart, &outcome);
                Some(restart)
            })
            .await
            .flatten();
        for done in restart.into_iter().flat_map(|restart| restart.answers) {
            // The request may have gone with its client.
            let _ = done.send(outcome.clone().map_err(|reason| anyhow!(reason)));
        }
    }
}

impl Agent {
    /// Drop the restart in progress, recording that it was cancelled.
    pub(super) fn cancel_restart(&mut self) {
        if let Some(restart) = self.restart.take() {
            self.end_restart(&restart, &Err("the restart was cancelled".into()));
        }
    }

    /// Record `outcome` for every run `restart` restarted the agent for.
    fn end_restart(&mut self, restart: &Restart, outcome: &Result<(), String>) {
        for run in restart.runs() {
            self.ended_restarts.insert(run, outcome.clone());
        }
    }

    /// How the restart of `of_run` that ended did, which also counts for
    /// `run`, joining it late.
    fn joined_late(&mut self, of_run: &Option<String>, run: Option<String>) -> Result<(), String> {
        let outcome = self
            .ended_restarts
            .get(of_run)
            .cloned()
            .unwrap_or_else(|| Err(format!("no restart of {} is in progress", self.name)));
        if run.is_some() {
            self.ended_restarts.insert(run, outcome.clone());
        }
        outcome
    }

    /// The runs a restart started the agent again for.
    fn restarted_by(&self) -> Vec<String> {
        let mut runs: Vec<String> = self
            .ended_restarts
            .iter()
            .filter(|(_, outcome)| outcome.is_ok())
            .filter_map(|(run, _)| run.clone())
            .collect();
        runs.sort();
        runs
    }
}

/// How the restart `answer` answers ended.
async fn restarted(answer: oneshot::Receiver<Result<()>>) -> Result<()> {
    answer
        .await
        .unwrap_or_else(|_| Err(anyhow!("the agent did not start again")))
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
