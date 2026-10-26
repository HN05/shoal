//! `shoal restart`: an agent stops and its wrapper starts its session again
//! in the same terminal, as after a protection stop, but at once and outside
//! the agent queue, since the agent keeps its slot.
use anyhow::{Context, Result, anyhow, ensure};
use tokio::sync::oneshot;

use super::Manager;
use crate::{daemon::store, model::RunningAgent};

/// A restart in progress, which answers its request.
pub(super) struct Restart {
    done: oneshot::Sender<Result<(), String>>,
    /// The stopped agent was resumed, so its wrapper's next start is the
    /// restarted agent rather than the first one.
    resumed: bool,
}

impl Restart {
    fn new() -> (Self, oneshot::Receiver<Result<(), String>>) {
        let (done, answer) = oneshot::channel();
        let restart = Self {
            done,
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
        let connections = self.connections.lock().await;
        let agents = self.agents.lock().await;
        Ok(executions
            .into_iter()
            .filter_map(|execution| {
                let (agent, restorable) = match agents.get(&execution.id) {
                    Some(agent) => (Some(agent.name.clone()), agent.recover),
                    None if connections
                        .get(&execution.id)
                        .is_some_and(|sender| !sender.is_closed()) =>
                    {
                        return None;
                    }
                    None => (None, false),
                };
                Some(RunningAgent {
                    execution: execution.id,
                    workspace_id: execution.workspace_id,
                    agent,
                    restorable,
                })
            })
            .collect())
    }

    /// Restart the agent of `execution`, returning once it runs again.
    pub(crate) async fn restart_agent(&self, execution: &str) -> Result<()> {
        let restarted = self.request_restart(execution).await?;
        restarted
            .await
            .unwrap_or_else(|_| Err("the agent did not start again".into()))
            .map_err(|reason| anyhow!(reason))
    }

    /// Ask the agent's wrapper to stop it for a restart.
    async fn request_restart(&self, id: &str) -> Result<oneshot::Receiver<Result<(), String>>> {
        let mut agents = self.agents.lock().await;
        let agent = agents
            .get_mut(id)
            .with_context(|| format!("no connected agent runs execution {id}"))?;
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
        let (restart, answer) = Restart::new();
        agent.restart = Some(restart);
        if agent.stop.send(true).is_err() {
            agent.restart = None;
            anyhow::bail!("{} disconnected", agent.name);
        }
        Ok(answer)
    }

    /// Whether `shoal restart` is restarting the agent of `id`.
    pub(crate) async fn restart_requested(&self, id: &str) -> bool {
        self.agents
            .lock()
            .await
            .get(id)
            .is_some_and(|agent| agent.restart.is_some())
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

    async fn answer_restart(&self, id: &str, once_resumed: bool, outcome: Result<(), String>) {
        let restart = self.agents.lock().await.get_mut(id).and_then(|agent| {
            agent
                .restart
                .take_if(|restart| restart.resumed || !once_resumed)
        });
        if let Some(restart) = restart {
            // The request may have gone with its client.
            let _ = restart.done.send(outcome);
        }
    }
}

#[cfg(test)]
mod tests;
