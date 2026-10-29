//! `shoal restart`: an agent stops and its wrapper starts its session again
//! in the same terminal, as after a protection stop, but at once and outside
//! the agent queue, since the agent keeps its slot.
use anyhow::{Context, Result, anyhow, ensure};
use tokio::sync::oneshot;

use super::{Agent, Manager};
use crate::{
    daemon::store, execution::recovery, model::RunningAgent, protocol::Protection,
    state::AgentState,
};

/// A restart in progress, which answers its request.
pub(super) struct Restart {
    /// Taken by the answer, which may come before the restart ends.
    done: Option<oneshot::Sender<Result<()>>>,
    /// The stopped agent was resumed, so its wrapper's next start is the
    /// restarted agent rather than the first one.
    resumed: bool,
}

impl Restart {
    fn new() -> (Self, oneshot::Receiver<Result<()>>) {
        let (done, answer) = oneshot::channel();
        let restart = Self {
            done: Some(done),
            resumed: false,
        };
        (restart, answer)
    }

    /// A restart whose request ended with the daemon that took it.
    pub(super) fn unanswered() -> Self {
        Self::new().0
    }

    pub(super) fn resume(&mut self) {
        self.resumed = true;
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
                        let (agent, restorable, state, interrupted) =
                            match agents.get(&execution.id) {
                                // Named as `--agent` and recovery records name it.
                                Some(agent) => (
                                    Some(agent.name.replace(' ', "-")),
                                    agent.recover,
                                    agent.turn,
                                    agent.interrupted,
                                ),
                                None if connections.is_connected(&execution.id) => return None,
                                None => (None, false, None, false),
                            };
                        Some(RunningAgent {
                            state,
                            interrupted,
                            execution: execution.id,
                            workspace_id: execution.workspace_id,
                            agent,
                            restorable,
                            origin: execution.origin,
                        })
                    })
                    .collect()
            })
            .await)
    }

    /// Restart the agent of `execution`, returning once it runs again.
    pub(crate) async fn restart_agent(&self, execution: &str) -> Result<()> {
        let restarted = self.request_restart(execution).await?;
        restarted
            .await
            .unwrap_or_else(|_| Err(anyhow!("the agent did not start again")))
    }

    /// Ask the agent's wrapper to stop it for a restart.
    async fn request_restart(&self, id: &str) -> Result<oneshot::Receiver<Result<()>>> {
        self.executions
            .with_agents(|agents| self.stop_for_restart(id, agents.get_mut(id)))
            .await
    }

    /// Save the handoff of `agent`, the agent of `id`, then stop it for a
    /// restart.
    fn stop_for_restart(
        &self,
        id: &str,
        agent: Option<&mut Agent>,
    ) -> Result<oneshot::Receiver<Result<()>>> {
        let agent = agent.with_context(|| format!("no connected agent runs execution {id}"))?;
        ensure!(
            agent.recover,
            "{} has no command to resume its session; configure [agent_resume] for it",
            agent.name
        );
        ensure!(
            !*agent.stop.borrow() && agent.restart.is_none(),
            "{} is already stopping",
            agent.name
        );
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
        let (restart, answer) = Restart::new();
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
        self.answer_restart(id, false, Err(anyhow!(reason))).await;
    }

    /// The restart of `id` waits for the stopped agent's processes to exit,
    /// which may take until the user ends them. Its request is answered with
    /// `issue` now, while the restart continues.
    pub(crate) async fn restart_waits(&self, id: &str, issue: String) {
        let done = self
            .executions
            .with_agent(id, |agent| agent.restart.as_mut()?.done.take())
            .await
            .flatten();
        if let Some(done) = done {
            // The request may have gone with its client.
            let _ = done.send(Err(RestartWaits(issue).into()));
        }
    }

    async fn answer_restart(&self, id: &str, once_resumed: bool, outcome: Result<()>) {
        let done = self
            .executions
            .with_agent(id, |agent| {
                agent
                    .restart
                    .take_if(|restart| restart.resumed || !once_resumed)
            })
            .await
            .flatten()
            .and_then(|restart| restart.done);
        if let Some(done) = done {
            // The request may have gone with its client.
            let _ = done.send(outcome);
        }
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
