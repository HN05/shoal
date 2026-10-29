//! Stop idle agents to free memory, saving each as protection does so that an
//! update restores it. Only an agent whose prompt cache has expired stops,
//! since restoring one rewrites its cache.
use std::collections::HashMap;

use anyhow::{Result, ensure};

use super::{Manager, Protection, StoppedAgent, Turn, still_idle};
use crate::state::AgentState;

/// A connected agent idle with no prompt cache left.
struct Candidate {
    execution: String,
    workspace_id: String,
    workspace: String,
    /// The agent's [`super::Agent::turns_started`] when it was chosen.
    turns_started: u64,
}

/// An agent to stop, and the turns it had started when it was chosen.
struct Chosen {
    protection: Protection,
    turns_started: u64,
}

impl Manager {
    /// Stop each idle agent whose prompt cache has expired in a done
    /// workspace whose cleanup is held, which could otherwise run until the
    /// workspace goes, or in any workspace whose `[idle_stop]` is enabled. A
    /// failure in one workspace does not skip the others.
    pub(crate) async fn stop_idle_agents(&self) -> Result<()> {
        // The process scan that tells agents with work in flight waits until
        // some agent would stop.
        let turns = self.reported_turns().await?;
        let now = i64::try_from(crate::time::unix_seconds())?;
        let mut chosen = HashMap::new();
        let mut failed = Vec::new();
        for candidate in self.idle_candidates(&turns, now).await {
            match self.idle_stop(&candidate).await {
                Ok(Some(protection)) => {
                    let turns_started = candidate.turns_started;
                    let stop = Chosen {
                        protection,
                        turns_started,
                    };
                    chosen.insert(candidate.execution, stop);
                }
                Ok(None) => {}
                Err(error) => failed.push(format!("{}: {error:#}", candidate.workspace)),
            }
        }
        if !chosen.is_empty() {
            let turns = self.agent_turns().await?;
            let now = i64::try_from(crate::time::unix_seconds())?;
            self.stop_idle(turns, chosen, now).await;
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
                    .filter(|(id, agent)| {
                        agent.running()
                            && agent.recover
                            && turns
                                .get(*id)
                                .is_some_and(|turn| turn.idle_without_cache(now))
                    })
                    .map(|(id, agent)| Candidate {
                        execution: id.clone(),
                        workspace_id: agent.workspace_id.clone(),
                        workspace: agent.workspace.clone(),
                        turns_started: agent.turns_started,
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
        Ok(settings.idle_stop.enabled.then(Protection::idle_expired))
    }

    /// Stop the `chosen` agents that are still running and idle with no
    /// prompt cache left at `now`, as `turns` read before taking the agents
    /// lock and their own reports since show: a turn that came since they
    /// were chosen renewed the cache, even one that ended while `turns`
    /// were read.
    async fn stop_idle(
        &self,
        mut turns: HashMap<String, Turn>,
        chosen: HashMap<String, Chosen>,
        now: i64,
    ) {
        let stopped: Vec<StoppedAgent> = self
            .executions
            .with_agents(|agents| {
                still_idle(&mut turns, agents);
                chosen
                    .into_iter()
                    .filter(|(id, _)| {
                        turns
                            .get(id)
                            .is_some_and(|turn| turn.idle_without_cache(now))
                    })
                    .filter_map(|(id, stop)| {
                        let agent = agents.get_mut(&id).filter(|agent| {
                            agent.running() && agent.turns_started == stop.turns_started
                        })?;
                        self.protect(&id, agent, &stop.protection, Some(AgentState::Idle))
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
