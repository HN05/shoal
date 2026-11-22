//! Put idle agents to sleep to make room for queued work, before their wait
//! for the user ends or where `[sleep]` keeps them running: while an entry
//! waits for a slot or memory, an agent idle for the least wait whose
//! priority is no higher than the entry's goes to sleep, first in the stop
//! order first, and only one that holds a slot for an entry waiting for
//! one. One the user is engaged with stays running, as do light agents, a
//! workspace's or a chat's, and one with a running agent below it. A woken agent reports no idle turn until it has worked one, so it
//! finishes that turn before it can sleep for room again.
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_schedule::priority::Priority;
use std::collections::HashMap;

use anyhow::Result;

use super::{
    Manager, Protection, ProtectionExt,
    idle_stop::{Candidate, Chosen},
};
use crate::daemon::turns::Turn;
use crate::{
    config::MachineLimit, daemon::engagement::FLOOR, model::HeldBy, state::QueueState,
    time::UnixTime,
};

/// A queued entry waiting for room, with its priority and what it needs.
struct Waiting {
    priority: Priority,
    needs: Room,
}

/// What a queued entry waits for.
enum Room {
    /// A slot of the machine, or of the repository whose limit holds it.
    Slot {
        repository: Option<RepositoryId>,
    },
    Memory,
}

/// An idle agent that may sleep to make room.
struct Sleeper<'a> {
    candidate: &'a Candidate,
    /// Its space's repository; none for a chat without one.
    repository: Option<RepositoryId>,
    /// Whether it holds a slot, which an agent of a done space does not.
    holds_slot: bool,
    priority: Priority,
}

impl Sleeper<'_> {
    /// Whether sleeping makes room for `entry`: its priority is no higher,
    /// and it frees what the entry waits for, a slot of the entry's
    /// repository when that repository's limit holds it.
    fn serves(&self, entry: &Waiting) -> bool {
        let frees = match &entry.needs {
            Room::Memory => true,
            Room::Slot { repository } => {
                self.holds_slot
                    && (repository.as_ref())
                        .is_none_or(|repository| self.repository.as_ref() == Some(repository))
            }
        };
        self.priority <= entry.priority && frees
    }
}

impl Manager {
    /// The agents among the idle `candidates`, besides those already
    /// `chosen`, to put to sleep at `now` for the entries waiting for room:
    /// for each entry in queue order, the lowest-ranked agent that serves it.
    pub(super) async fn make_room(
        &self,
        candidates: &[Candidate],
        chosen: &HashMap<ExecutionId, Chosen>,
        now: UnixTime,
    ) -> Result<HashMap<ExecutionId, Chosen>> {
        let waiting = self.waiting_for_room().await?;
        let mut room = HashMap::new();
        if waiting.is_empty() {
            return Ok(room);
        }
        let mut sleepers = self.sleepers(candidates, chosen, now).await?;
        for entry in &waiting {
            let Some(index) = sleepers.iter().position(|sleeper| sleeper.serves(entry)) else {
                continue;
            };
            let sleeper = sleepers.remove(index);
            let stop = Chosen {
                protection: Protection::making_room(),
                turn: sleeper.candidate.turn,
                interacted_at: sleeper.candidate.interacted_at,
            };
            room.insert(sleeper.candidate.execution.clone(), stop);
        }
        Ok(room)
    }

    /// The queued entries that a slot or memory holds back, in queue order,
    /// leaving out those waiting for a machine slot where the machine has
    /// none, which no sleep frees.
    async fn waiting_for_room(&self) -> Result<Vec<Waiting>> {
        let entries = self.queue_entries().await?;
        let slots = self.config().machine.max_heavy_agents != MachineLimit::Fixed(0);
        Ok((entries.into_iter())
            .filter(|entry| entry.state == QueueState::Queued)
            .filter_map(|entry| {
                let needs = match entry.held_by? {
                    HeldBy::RepositoryLimit => Room::Slot {
                        repository: Some(entry.repository_id?),
                    },
                    HeldBy::MaxHeavyAgents if slots => Room::Slot { repository: None },
                    HeldBy::Memory { .. } | HeldBy::MemoryKept => Room::Memory,
                    _ => return None,
                };
                let priority = entry.rank.map(|rank| rank.priority).unwrap_or_default();
                Some(Waiting { priority, needs })
            })
            .collect())
    }

    /// The `candidates` not already `chosen` that may sleep for room at
    /// `now`, in stop order: heavy agents, a workspace's or a chat's, idle
    /// for the least wait, that the user is not engaged with and that no
    /// running agent works below.
    async fn sleepers<'a>(
        &self,
        candidates: &'a [Candidate],
        chosen: &HashMap<ExecutionId, Chosen>,
        now: UnixTime,
    ) -> Result<Vec<Sleeper<'a>>> {
        let turns: HashMap<ExecutionId, Turn> = (candidates.iter())
            .map(|candidate| (candidate.execution.clone(), candidate.turn))
            .collect();
        let order = self.stop_order(&turns).await;
        let mut eligible = Vec::new();
        for stop in order.stops() {
            let Some(candidate) = (stop.agent()).and_then(|id| {
                candidates
                    .iter()
                    .find(|candidate| candidate.execution == *id)
            }) else {
                continue;
            };
            let settling = candidate.turn.since + FLOOR > now;
            let below = order.with_agents_below(stop).len() > 1;
            if chosen.contains_key(&candidate.execution) || candidate.light || settling || below {
                continue;
            }
            if !self.agents.engaged(&candidate.workspace_id).await? {
                eligible.push(candidate);
            }
        }
        let ids: Vec<SpaceId> = (eligible.iter())
            .map(|candidate| candidate.workspace_id.clone())
            .collect();
        let priorities: HashMap<SpaceId, Priority> =
            self.ranked_spaces(&ids).await?.into_iter().collect();
        let mut sleepers = Vec::new();
        for candidate in eligible {
            let space = self.space_by_id(&candidate.workspace_id).await?;
            sleepers.push(Sleeper {
                candidate,
                repository: space.repository_id.as_ref().cloned(),
                holds_slot: !space.done(),
                priority: (priorities.get(&candidate.workspace_id).copied()).unwrap_or_default(),
            });
        }
        Ok(sleepers)
    }
}

#[cfg(test)]
mod tests;
