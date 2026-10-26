//! `shoal restart`: an agent stops and its wrapper starts its session again
//! in the same terminal, as after a protection stop, but at once and outside
//! the agent queue, since the agent keeps its slot.
use anyhow::{Context, Result, anyhow, ensure};
use tokio::sync::oneshot;

use super::Manager;
use crate::{daemon::store, model::RunningAgent};

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
        let (sender, receiver) = oneshot::channel();
        agent.restart = Some(sender);
        if agent.stop.send(true).is_err() {
            agent.restart = None;
            anyhow::bail!("{} disconnected", agent.name);
        }
        Ok(receiver)
    }

    /// Whether `shoal restart` is restarting the agent of `id`.
    pub(crate) async fn restart_requested(&self, id: &str) -> bool {
        self.agents
            .lock()
            .await
            .get(id)
            .is_some_and(|agent| agent.restart.is_some())
    }

    /// Answer the restart of `id`: `Ok` once the agent runs again.
    pub(crate) async fn finish_restart(&self, id: &str, outcome: Result<(), String>) {
        let done = self
            .agents
            .lock()
            .await
            .get_mut(id)
            .and_then(|agent| agent.restart.take());
        if let Some(done) = done {
            // The request may have gone with its client.
            let _ = done.send(outcome);
        }
    }
}

#[cfg(test)]
mod tests;
