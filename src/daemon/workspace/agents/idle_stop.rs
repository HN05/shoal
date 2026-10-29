//! Stop idle agents to free memory, saving each as protection does so that an
//! update restores it. Only an agent whose prompt cache has expired stops,
//! since restoring one rewrites its cache.
use std::{collections::HashMap, time::Duration};

use anyhow::{Result, ensure};

use super::{Manager, Protection, StoppedAgent, Turn, still_idle};
use crate::state::AgentState;

/// A connected agent idle with no prompt cache left.
struct Candidate {
    execution: String,
    workspace_id: String,
    workspace: String,
    /// How long it has been idle.
    idle: Duration,
}

impl Manager {
    /// Stop each idle agent whose prompt cache has expired in a done
    /// workspace whose cleanup is held, which could otherwise run until the
    /// workspace goes, or idle longer than its workspace's `[idle_stop]`
    /// allows. A failure in one workspace does not skip the others.
    pub(crate) async fn stop_idle_agents(&self) -> Result<()> {
        let turns = self.agent_turns().await?;
        let now = i64::try_from(crate::time::unix_seconds())?;
        let mut chosen = HashMap::new();
        let mut failed = Vec::new();
        for candidate in self.idle_candidates(&turns, now).await {
            match self.idle_stop(&candidate).await {
                Ok(Some(protection)) => {
                    chosen.insert(candidate.execution, protection);
                }
                Ok(None) => {}
                Err(error) => failed.push(format!("{}: {error:#}", candidate.workspace)),
            }
        }
        if !chosen.is_empty() {
            self.stop_idle(turns, chosen).await;
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    /// The running agents with a restore command that `turns` shows idle
    /// with no prompt cache left at `now`.
    async fn idle_candidates(&self, turns: &HashMap<String, Turn>, now: i64) -> Vec<Candidate> {
        self.executions
            .with_agents(|agents| {
                agents
                    .iter()
                    .filter(|(_, agent)| agent.running() && agent.recover)
                    .filter_map(|(id, agent)| {
                        let turn = turns.get(id)?;
                        turn.idle_without_cache(now).then(|| Candidate {
                            execution: id.clone(),
                            workspace_id: agent.workspace_id.clone(),
                            workspace: agent.workspace.clone(),
                            idle: Duration::from_secs(
                                u64::try_from(now - turn.since).unwrap_or_default(),
                            ),
                        })
                    })
                    .collect()
            })
            .await
    }

    /// Why the `candidate` agent stops, if it does.
    async fn idle_stop(&self, candidate: &Candidate) -> Result<Option<Protection>> {
        let id = &candidate.workspace_id;
        if self.completion(id).await?.is_some() && self.keep_reason(id).await?.is_some() {
            return Ok(Some(Protection::idle_held()));
        }
        let settings = self.workspace_settings(&self.workspace(id).await?).await?;
        let timed_out = settings
            .idle_stop
            .after()
            .is_some_and(|after| candidate.idle >= after);
        Ok(timed_out.then(Protection::idle_timeout))
    }

    /// Stop the `chosen` agents that are still running and idle, as `turns`
    /// read before taking the agents lock and their own reports since show.
    async fn stop_idle(
        &self,
        mut turns: HashMap<String, Turn>,
        chosen: HashMap<String, Protection>,
    ) {
        let stopped: Vec<StoppedAgent> = self
            .executions
            .with_agents(|agents| {
                still_idle(&mut turns, agents);
                chosen
                    .into_iter()
                    .filter(|(id, _)| turns.get(id).is_some_and(Turn::idle))
                    .filter_map(|(id, protection)| {
                        let agent = agents.get_mut(&id).filter(|agent| agent.running())?;
                        self.protect(&id, agent, &protection, Some(AgentState::Idle))
                    })
                    .collect()
            })
            .await;
        for agent in &stopped {
            self.notify_protection_stop(agent).await;
        }
    }
}

#[cfg(test)]
mod tests;
