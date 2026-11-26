//! Putting idle agents to sleep, each saved as protection saves it so that an
//! update restores it. `docs/design/agent-lifecycle.md` says which agents sleep
//! and when.
use crate::control::ProtectionExt;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;
use std::collections::HashMap;

use anyhow::Result;

use super::{Protection, StoppedAgent, Turn};
use crate::Agents;
use crate::daemon::Daemon;
use crate::time::UnixTime;

/// A connected idle agent.
pub struct Candidate {
    pub execution: ExecutionId,
    pub workspace_id: SpaceId,
    pub workspace: SpaceName,
    /// Whether it is light, so that only its space's blocking stops it.
    pub light: bool,
    /// The idle turn it was found in.
    pub turn: Turn,
    /// When the user had last interacted with it as it was found.
    pub interacted_at: Option<UnixTime>,
}

/// An agent to put to sleep, the idle turn it was chosen in, and when the
/// user had last interacted with it then, which a later interaction makes
/// stale.
pub struct Chosen {
    pub protection: Protection,
    pub turn: Turn,
    pub interacted_at: Option<UnixTime>,
}

impl Chosen {
    /// The user's interaction as it was chosen, for a stop the user keeps
    /// the agent from, which is every one but a blocked space's.
    fn kept_by(&self) -> Option<super::stops::Interacted> {
        (!self.protection.is_blocked()).then_some(super::stops::Interacted(self.interacted_at))
    }
}

impl Agents {
    /// Put to sleep, at `now`, each idle agent whose space is blocked, each
    /// heavy one whose wait for the user has passed, and those that make room
    /// for queued work. A failure in one space does not skip the others.
    pub async fn stop_idle_agents(&self, daemon: &dyn Daemon, now: UnixTime) -> Result<()> {
        // The process scan that tells agents with work in flight waits until
        // some agent would stop.
        let turns = self.reported_turns().await?;
        let candidates = self.idle_candidates(daemon, &turns).await?;
        let mut chosen = HashMap::new();
        let mut failed = shoal_daemon_base::failures::SpaceFailures::default();
        for candidate in &candidates {
            match self.idle_stop(daemon, candidate, now).await {
                Ok(Some(stop)) => {
                    chosen.insert(candidate.execution.clone(), stop);
                }
                Ok(None) => {}
                Err(error) => failed.push(&candidate.workspace_id, &candidate.workspace, &error),
            }
        }
        match self.make_room(daemon, &candidates, &chosen, now).await {
            Ok(room) => chosen.extend(room),
            Err(error) => crate::log!("no room made for queued work: {error:#}"),
        }
        self.sleep_for_waiting_retries(&candidates, &mut chosen)
            .await;
        if !chosen.is_empty() {
            let turns = self.agent_turns().await?;
            self.stop_idle(daemon, turns, chosen).await;
        }
        failed.into_result()
    }

    /// The running agents with a restore command that `turns` shows idle.
    async fn idle_candidates(
        &self,
        daemon: &dyn Daemon,
        turns: &HashMap<ExecutionId, Turn>,
    ) -> Result<Vec<Candidate>> {
        let light = self.light(daemon).await?;
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
    async fn idle_stop(
        &self,
        daemon: &dyn Daemon,
        candidate: &Candidate,
        now: UnixTime,
    ) -> Result<Option<Chosen>> {
        let id = &candidate.workspace_id;
        let protection = if daemon.space_blocked(id).await? {
            Protection::blocked()
        } else if !candidate.light
            && (self.sleeps_at(daemon, id, candidate.turn.since).await?).is_some_and(|at| at <= now)
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
                    crate::log!("turn retry of {space} unread for its stop: {error:#}");
                }
            }
        }
    }

    /// Stop the `chosen` agents still idle in the turn they were chosen in,
    /// unless the user interacted since or `turns` found work in flight outside
    /// their process group; a blocked space's agent stops all the same. A
    /// blocked agent's stop notifies no one, since its space's blocking already
    /// did.
    pub async fn stop_idle(
        &self,
        daemon: &dyn Daemon,
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
                false => self.notify_protection_stop(daemon, agent).await,
            }
        }
    }

    /// Leave the agent `agent`, stopped because its space is blocked, a
    /// message its restore finds waiting, so the restore starts its turn
    /// whatever cleared the space.
    async fn leave_blocked_message(&self, agent: &StoppedAgent) {
        let message = crate::queue::blockers::STOPPED_BLOCKED.to_owned();
        let kind = shoal_daemon_base::alerts::AlertKind::SpaceBlocked;
        if let Err(error) = (self.queue_agent_message(&agent.workspace_id, kind, message)).await {
            crate::log!("blocked agent {} untold: {error:#}", agent.id);
        }
    }
}

#[cfg(test)]
mod tests;
