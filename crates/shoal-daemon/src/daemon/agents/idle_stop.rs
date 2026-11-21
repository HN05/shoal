//! Put idle agents to sleep, saving each as protection does, so that an
//! update restores it: a heavy agent once it has waited for the user as
//! long as their engagement with it says, one that makes room for queued
//! work, and any whose space is blocked, once its turn ends, so that its
//! restore waits until nothing blocks the space. An agent whose turn retry
//! waits sleeps for the limit or failure it waits out, which its retry's
//! message ends. Stopping and restoring an agent costs no more prompt cache
//! than keeping it running would save.
use crate::daemon::agents::ProtectionExt;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;
use std::collections::HashMap;

use anyhow::Result;

use super::{Manager, Protection, StoppedAgent, Turn};
use crate::time::UnixTime;

/// A connected idle agent.
pub(super) struct Candidate {
    pub(super) execution: ExecutionId,
    pub(super) workspace_id: SpaceId,
    pub(super) workspace: SpaceName,
    /// Whether it is light, so that only its space's blocking stops it.
    pub(super) light: bool,
    /// The idle turn it was found in.
    pub(super) turn: Turn,
    /// When the user had last interacted with it as it was found.
    pub(super) interacted_at: Option<UnixTime>,
}

/// An agent to put to sleep, the idle turn it was chosen in, and when the
/// user had last interacted with it then, which a later interaction makes
/// stale.
pub(super) struct Chosen {
    pub(super) protection: Protection,
    pub(super) turn: Turn,
    pub(super) interacted_at: Option<UnixTime>,
}

impl Chosen {
    /// The user's interaction as it was chosen, for a stop the user keeps
    /// the agent from, which is every one but a blocked space's.
    fn kept_by(&self) -> Option<super::stops::Interacted> {
        (!self.protection.is_blocked()).then_some(super::stops::Interacted(self.interacted_at))
    }
}

impl Manager {
    /// Put to sleep, at `now`, each idle agent whose space is blocked; each
    /// heavy one whose wait for the user has passed; and those that make
    /// room for queued work, each for its pending turn retry where it has
    /// one. A failure in one space does not skip the others.
    pub async fn stop_idle_agents(&self, now: UnixTime) -> Result<()> {
        // The process scan that tells agents with work in flight waits until
        // some agent would stop.
        let turns = self.reported_turns().await?;
        let candidates = self.idle_candidates(&turns).await?;
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
        self.sleep_for_waiting_retries(&candidates, &mut chosen)
            .await;
        if !chosen.is_empty() {
            let turns = self.agent_turns().await?;
            self.stop_idle(turns, chosen).await;
        }
        failed.into_result()
    }

    /// The running agents with a restore command that `turns` shows idle.
    async fn idle_candidates(&self, turns: &HashMap<ExecutionId, Turn>) -> Result<Vec<Candidate>> {
        let light = self.light().await?;
        let interactions = self.interactions().await?;
        Ok(self
            .executions
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
                            light: light.execution(id),
                            turn,
                            interacted_at: interactions.get(&agent.workspace_id).copied(),
                        })
                    })
                    .collect()
            })
            .await)
    }

    /// Why the `candidate` agent sleeps at `now`, if it does: its space is
    /// blocked; or, for a heavy agent, its wait for the user has passed.
    async fn idle_stop(&self, candidate: &Candidate, now: UnixTime) -> Result<Option<Chosen>> {
        let id = &candidate.workspace_id;
        let protection = if self.space_blocked(id).await? {
            Protection::blocked()
        } else if !candidate.light
            && (self.sleeps_at(id, candidate.turn.since).await?).is_some_and(|at| at <= now)
        {
            Protection::away()
        } else {
            return Ok(None);
        };
        Ok(Some(Chosen {
            protection,
            turn: candidate.turn,
            interacted_at: candidate.interacted_at,
        }))
    }

    /// Have each of the `chosen` agents whose turn retry waits sleep for
    /// the limit or failure the retry waits out, so its stop names it and
    /// what wakes it, unless its space is blocked, whose clearing restores
    /// it. A retry that cannot be read leaves the stop as it was.
    async fn sleep_for_waiting_retries(
        &self,
        candidates: &[Candidate],
        chosen: &mut HashMap<ExecutionId, Chosen>,
    ) {
        for candidate in candidates {
            let Some(stop) = chosen.get_mut(&candidate.execution) else {
                continue;
            };
            if stop.protection.is_blocked() {
                continue;
            }
            match self.waiting_turn_retry(&candidate.workspace_id).await {
                Ok(Some(failure)) => stop.protection = Protection::retrying(failure),
                Ok(None) => {}
                Err(error) => {
                    let space = &candidate.workspace;
                    crate::daemon::log!("turn retry of {space} unread for its stop: {error:#}");
                }
            }
        }
    }

    /// Stop the `chosen` agents that are still idle in the turn they were
    /// chosen in, that the user has not interacted with since, unless their
    /// space is blocked, and whose processes `turns` found no work in
    /// flight outside their process group. A blocked agent's stop notifies no one; its space's blocking
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
            let protected = self.protect(&id, &stop.protection, Some(stop.turn), stop.kept_by());
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
        let kind = crate::daemon::alerts::AlertKind::SpaceBlocked;
        if let Err(error) = (self.queue_agent_message(&agent.workspace_id, kind, message)).await {
            crate::daemon::log!("blocked agent {} untold: {error:#}", agent.id);
        }
    }
}

#[cfg(test)]
mod tests;
