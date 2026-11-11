//! Stop idle agents, saving each as protection does: to free memory, so
//! that an update restores it, and whose space is blocked, so that its
//! restore waits until nothing blocks the space. Only an agent whose prompt
//! cache has expired stops to free memory, since restoring one rewrites its
//! cache; a blocked one stops once its turn ends.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceName;
use crate::daemon::workspace::agents::ProtectionExt;
use std::collections::HashMap;

use anyhow::Result;

use super::{Manager, Protection, StoppedAgent, Turn};

/// A connected idle agent.
struct Candidate {
    execution: ExecutionId,
    workspace_id: SpaceId,
    workspace: SpaceName,
    /// The idle turn it was found in.
    turn: Turn,
}

/// An agent to stop, and the idle turn it was chosen in.
struct Chosen {
    protection: Protection,
    turn: Turn,
}

impl Manager {
    /// Stop each idle agent whose space is blocked, and each whose prompt
    /// cache has expired in a done workspace whose cleanup is held, which
    /// could otherwise run until the workspace goes, or in any workspace
    /// whose `[idle_stop]` is enabled. A failure in one workspace does not
    /// skip the others.
    pub(crate) async fn stop_idle_agents(&self) -> Result<()> {
        // The process scan that tells agents with work in flight waits until
        // some agent would stop.
        let turns = self.reported_turns().await?;
        let now = crate::time::UnixTime::now();
        let mut chosen = HashMap::new();
        let mut failed = crate::daemon::cleanup::SpaceFailures::default();
        for candidate in self.idle_candidates(&turns).await {
            let cache_expired = candidate.turn.idle_without_cache(now);
            match self.idle_stop(&candidate, cache_expired).await {
                Ok(Some(protection)) => {
                    let turn = candidate.turn;
                    chosen.insert(candidate.execution, Chosen { protection, turn });
                }
                Ok(None) => {}
                Err(error) => failed.push(&candidate.workspace_id, &candidate.workspace, &error),
            }
        }
        if !chosen.is_empty() {
            let turns = self.agent_turns().await?;
            let now = crate::time::UnixTime::now();
            self.stop_idle(turns, chosen, now).await;
        }
        failed.into_result()
    }

    /// The running agents with a restore command that `turns` shows idle.
    async fn idle_candidates(&self, turns: &HashMap<ExecutionId, Turn>) -> Vec<Candidate> {
        self.executions
            .with_agents(|agents| {
                agents
                    .iter()
                    .filter(|(_, agent)| agent.running() && agent.recover)
                    .filter_map(|(id, agent)| {
                        let turn = turns.get(id).copied().filter(Turn::idle)?;
                        Some(Candidate {
                            execution: id.clone(),
                            workspace_id: agent.workspace_id.clone(),
                            workspace: agent.workspace.clone(),
                            turn,
                        })
                    })
                    .collect()
            })
            .await
    }

    /// Why the `candidate` agent stops, if it does: its space is blocked,
    /// or, once its prompt cache expired, to free memory.
    async fn idle_stop(
        &self,
        candidate: &Candidate,
        cache_expired: bool,
    ) -> Result<Option<Protection>> {
        let id = &candidate.workspace_id;
        if self.space_blocked(id).await? {
            return Ok(Some(Protection::blocked()));
        }
        if !cache_expired {
            return Ok(None);
        }
        if self.completion(id).await?.is_some() && self.keep_reason(id).await?.is_some() {
            return Ok(Some(Protection::idle_held()));
        }
        let settings = self
            .workspace_settings(&self.workspace_by_id(id).await?)
            .await?;
        Ok(settings.idle_stop.enabled.then(Protection::idle_expired))
    }

    /// Stop the `chosen` agents that are still idle in the turn they were
    /// chosen in, with no prompt cache left at `now` unless their space is
    /// blocked, and whose processes `turns` found no work in flight outside
    /// their process group. A blocked agent's stop notifies no one; its
    /// space's blocking already did.
    async fn stop_idle(
        &self,
        turns: HashMap<ExecutionId, Turn>,
        chosen: HashMap<ExecutionId, Chosen>,
        now: crate::time::UnixTime,
    ) {
        let mut stopped: Vec<StoppedAgent> = Vec::new();
        for (id, stop) in chosen {
            if !turns.get(&id).is_some_and(Turn::idle) {
                continue;
            }
            let cache_expired_at = (!stop.protection.is_blocked()).then_some(now);
            let protected = self.protect(&id, &stop.protection, Some(stop.turn), cache_expired_at);
            stopped.extend(protected.await);
        }
        for agent in &stopped {
            match agent.protection.is_blocked() {
                true => self.leave_blocked_message(agent).await,
                false => self.notify_protection_stop(agent).await,
            }
        }
    }

    /// Leave the agent `agent`, stopped because its space is blocked, a
    /// message its restore finds waiting, so the restore starts its turn
    /// whatever cleared the space.
    async fn leave_blocked_message(&self, agent: &StoppedAgent) {
        let message = crate::daemon::blockers::STOPPED_BLOCKED.to_owned();
        if let Err(error) = self.queue_agent_message(&agent.workspace_id, message).await {
            crate::daemon::log!("blocked agent {} untold: {error:#}", agent.id);
        }
    }
}

#[cfg(test)]
mod tests;
