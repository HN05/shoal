//! `shoal restart`: an agent stops and its wrapper starts its session again
//! in the same terminal, as after a protection stop, but at once and outside
//! the agent queue, since the agent keeps its slot.
use anyhow::{Context, Result, anyhow, ensure};
use tokio::{sync::oneshot, time::Instant};

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
    requested: Instant,
    /// The stopped agent was resumed, so its wrapper's next start is the
    /// restarted agent rather than the first one.
    resumed: bool,
    /// Why the restart waits for the stopped agent's processes, which
    /// answers each request at once.
    waits: Option<String>,
}

impl Restart {
    /// A restart without requests to answer, such as one whose request ended
    /// with the daemon that took it.
    pub(super) fn unanswered() -> Self {
        Self {
            answers: Vec::new(),
            requested: Instant::now(),
            resumed: false,
            waits: None,
        }
    }

    /// Answer one more request: at once while the restart waits for the
    /// agent's processes, otherwise once it ends.
    fn answer(&mut self) -> oneshot::Receiver<Result<()>> {
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
            requested_ms_ago: millis(self.requested.elapsed()),
        }
    }
}

fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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
                            started_ms_ago: agent.map(|agent| millis(agent.started.elapsed())),
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
    pub(crate) async fn restart_agent(&self, execution: &str) -> Result<()> {
        restarted(self.request_restart(execution).await?).await
    }

    /// Wait for the restart of `execution` in progress as [`Self::restart_agent`]
    /// does; a restart that ended already left its agent running again, since
    /// one that failed ends with the agent's wrapper.
    pub(crate) async fn join_restart(&self, execution: &str) -> Result<()> {
        let joined = self
            .executions
            .with_agents(|agents| {
                let agent = agents
                    .get_mut(execution)
                    .with_context(|| format!("no connected agent runs execution {execution}"))?;
                anyhow::Ok(agent.restart.as_mut().map(Restart::answer))
            })
            .await?;
        match joined {
            Some(answer) => restarted(answer).await,
            None => Ok(()),
        }
    }

    /// Ask the agent's wrapper to stop it for a restart.
    async fn request_restart(&self, id: &str) -> Result<oneshot::Receiver<Result<()>>> {
        self.executions
            .with_agents(|agents| self.stop_for_restart(id, agents.get_mut(id)))
            .await
    }

    /// Save the handoff of `agent`, the agent of `id`, then stop it for a
    /// restart, or join the restart already in progress.
    fn stop_for_restart(
        &self,
        id: &str,
        agent: Option<&mut Agent>,
    ) -> Result<oneshot::Receiver<Result<()>>> {
        let agent = agent.with_context(|| format!("no connected agent runs execution {id}"))?;
        if let Some(restart) = &mut agent.restart {
            return Ok(restart.answer());
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
        let mut restart = Restart::unanswered();
        let answer = restart.answer();
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
                agent
                    .restart
                    .take_if(|restart| restart.resumed || !once_resumed)
            })
            .await
            .flatten();
        for done in restart.into_iter().flat_map(|restart| restart.answers) {
            // The request may have gone with its client.
            let _ = done.send(outcome.clone().map_err(|reason| anyhow!(reason)));
        }
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
