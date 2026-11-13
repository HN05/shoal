//! Put idle agents to sleep, saving each as protection does, so that an
//! update restores it: to free memory, after `shoal sleep`, and to make room
//! for queued work, and whose space is blocked, so that its restore waits
//! until nothing blocks the space. Only an agent whose prompt cache has
//! expired sleeps to free memory, and one that `shoal sleep` or queued work
//! puts to sleep only once that costs nothing; a blocked one sleeps once its
//! turn ends.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceName;
use crate::daemon::workspace::agents::ProtectionExt;
use std::collections::HashMap;

use anyhow::Result;

use super::{Manager, Protection, StoppedAgent, Turn};
use crate::time::UnixTime;

/// A connected idle agent.
pub(super) struct Candidate {
    pub(super) execution: ExecutionId,
    pub(super) workspace_id: SpaceId,
    pub(super) workspace: SpaceName,
    /// Whether the user attends its space.
    pub(super) attended: bool,
    /// The idle turn it was found in.
    pub(super) turn: Turn,
}

/// An agent to put to sleep, the idle turn it was chosen in, and the time
/// recording its stop must find its prompt cache expired by, if any.
pub(super) struct Chosen {
    pub(super) protection: Protection,
    pub(super) turn: Turn,
    pub(super) cache_expired_at: Option<UnixTime>,
}

impl Manager {
    /// Put to sleep each idle agent whose space is blocked; each whose
    /// prompt cache has expired in a done workspace whose cleanup is held,
    /// which could otherwise run until the workspace goes, or in any
    /// workspace whose `[sleep] idle` is enabled; each whose space sleeps
    /// after `shoal sleep` once that costs nothing; and those that make room
    /// for queued work. A failure in one workspace does not skip the others.
    pub(crate) async fn stop_idle_agents(&self) -> Result<()> {
        // The process scan that tells agents with work in flight waits until
        // some agent would stop.
        let turns = self.reported_turns().await?;
        let now = UnixTime::now();
        let candidates = self.idle_candidates(&turns).await;
        let mut chosen = HashMap::new();
        let mut failed = crate::daemon::cleanup::SpaceFailures::default();
        for candidate in &candidates {
            match self.idle_stop(candidate, now).await {
                Ok(Some(stop)) => {
                    chosen.insert(candidate.execution.clone(), stop);
                }
                Ok(None) => {}
                Err(error) => failed.push(&candidate.workspace_id, &candidate.workspace, &error),
            }
        }
        match self.make_room(&candidates, &chosen, now).await {
            Ok(room) => chosen.extend(room),
            Err(error) => crate::daemon::log!("no room made for queued work: {error:#}"),
        }
        if !chosen.is_empty() {
            let turns = self.agent_turns().await?;
            self.stop_idle(turns, chosen).await;
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
                            attended: agent.attended,
                            turn,
                        })
                    })
                    .collect()
            })
            .await
    }

    /// Why the `candidate` agent sleeps at `now`, if it does: its space is
    /// blocked; or, unless the user attends its space, its prompt cache
    /// expired where that frees memory, or its space sleeps and sleeping
    /// costs nothing.
    async fn idle_stop(&self, candidate: &Candidate, now: UnixTime) -> Result<Option<Chosen>> {
        let id = &candidate.workspace_id;
        let chosen = |protection, cache_expired_at| {
            Some(Chosen {
                protection,
                turn: candidate.turn,
                cache_expired_at,
            })
        };
        if self.space_blocked(id).await? {
            return Ok(chosen(Protection::blocked(), None));
        }
        if candidate.attended {
            return Ok(None);
        }
        if candidate.turn.idle_without_cache(now)
            && let Some(protection) = self.expired_stop(id).await?
        {
            return Ok(chosen(protection, Some(now)));
        }
        if self.space_by_id(id).await?.sleep().is_some()
            && let Some(free) = self.sleep_cost(id, &candidate.turn, now).await?
        {
            return Ok(chosen(Protection::slept(), free.cache_expired_at(now)));
        }
        Ok(None)
    }

    /// Why the idle agent of the space `id`, whose prompt cache expired,
    /// sleeps to free memory, if it does: its space is done and its cleanup
    /// held, or its `[sleep] idle` is enabled.
    async fn expired_stop(&self, id: &SpaceId) -> Result<Option<Protection>> {
        if self.completion(id).await?.is_some() && self.keep_reason(id).await?.is_some() {
            return Ok(Some(Protection::idle_held()));
        }
        let settings = self
            .workspace_settings(&self.workspace_by_id(id).await?)
            .await?;
        Ok(settings.sleep.idle.then(Protection::idle_expired))
    }

    /// Stop the `chosen` agents that are still idle in the turn they were
    /// chosen in, with no prompt cache left by the time each requires, and
    /// whose processes `turns` found no work in flight outside their process
    /// group. A blocked agent's stop notifies no one; its space's blocking
    /// already did.
    async fn stop_idle(
        &self,
        turns: HashMap<ExecutionId, Turn>,
        chosen: HashMap<ExecutionId, Chosen>,
    ) {
        let mut stopped: Vec<StoppedAgent> = Vec::new();
        for (id, stop) in chosen {
            if !turns.get(&id).is_some_and(Turn::idle) {
                continue;
            }
            let protected = self.protect(
                &id,
                &stop.protection,
                Some(stop.turn),
                stop.cache_expired_at,
            );
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
