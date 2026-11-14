//! Put idle agents to sleep to make room for queued work: while an entry
//! waits for a slot or memory, an idle agent that something will wake and
//! whose priority is no higher than the entry's goes to sleep, lowest rank
//! first, once that costs nothing, and only one that holds a slot for an
//! entry waiting for one. One that nothing would wake stays
//! running, as do light agents, a workspace's or a chat's. A woken agent
//! reports no idle turn until it has worked one, so it finishes that turn
//! before it can sleep for room again.
use crate::client::{
    execution_ids::ExecutionId, priority::Priority, repository_ids::RepositoryId,
    space_ids::SpaceId,
};
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;

use super::{
    Manager, Protection, ProtectionExt,
    idle_stop::{Candidate, Chosen},
    sleep_cost::Free,
};
use crate::{
    daemon::links::wait::watches_open_pr, model::HeldBy, state::QueueState, time::UnixTime,
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
    free: Free,
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
                cache_expired_at: sleeper.free.cache_expired_at(now),
            };
            room.insert(sleeper.candidate.execution.clone(), stop);
        }
        Ok(room)
    }

    /// The queued entries that a slot or memory holds back, in queue order.
    async fn waiting_for_room(&self) -> Result<Vec<Waiting>> {
        let entries = self.queue_entries().await?;
        Ok((entries.into_iter())
            .filter(|entry| entry.state == QueueState::Queued)
            .filter_map(|entry| {
                let needs = match entry.held_by? {
                    HeldBy::RepositoryLimit => Room::Slot {
                        repository: Some(entry.repository_id?),
                    },
                    HeldBy::MaxAgents => Room::Slot { repository: None },
                    HeldBy::Memory { .. } | HeldBy::MemoryKept => Room::Memory,
                    _ => return None,
                };
                let priority = entry.rank.map(|rank| rank.priority).unwrap_or_default();
                Some(Waiting { priority, needs })
            })
            .collect())
    }

    /// The `candidates` not already `chosen` that may sleep for room at
    /// `now`, lowest rank first: heavy agents, a workspace's or a chat's,
    /// which something will wake and for which sleeping costs nothing.
    async fn sleepers<'a>(
        &self,
        candidates: &'a [Candidate],
        chosen: &HashMap<ExecutionId, Chosen>,
        now: UnixTime,
    ) -> Result<Vec<Sleeper<'a>>> {
        let mut eligible = Vec::new();
        for candidate in candidates {
            let (execution, id) = (&candidate.execution, &candidate.workspace_id);
            if chosen.contains_key(execution) || candidate.light {
                continue;
            }
            if !self.awaits_wake(id).await? {
                continue;
            }
            if let Some(free) = self.sleep_cost(id, &candidate.turn, now).await? {
                eligible.push((candidate, free));
            }
        }
        let ids: Vec<SpaceId> = (eligible.iter())
            .map(|(candidate, _)| candidate.workspace_id.clone())
            .collect();
        let ranked = self.ranked_spaces(&ids).await?;
        let mut sleepers = Vec::new();
        for (id, priority) in ranked.into_iter().rev() {
            let Some(at) = eligible
                .iter()
                .position(|(candidate, _)| candidate.workspace_id == id)
            else {
                continue;
            };
            let (candidate, free) = eligible.remove(at);
            let space = self.space_by_id(&id).await?;
            sleepers.push(Sleeper {
                candidate,
                repository: space.repository_id().cloned(),
                holds_slot: !space.done(),
                priority,
                free,
            });
        }
        Ok(sleepers)
    }

    /// Whether something will wake the agent of the space `id` once it
    /// sleeps: an open PR it watches, a blocker, an open question or a
    /// request waiting for a decision.
    async fn awaits_wake(&self, id: &SpaceId) -> Result<bool> {
        let space = id.to_owned();
        if self
            .store
            .read(move |db| watches_or_asks(db, &space))
            .await?
        {
            return Ok(true);
        }
        Ok(self.space_blocked(id).await? || self.has_open_questions(id).await?)
    }
}

/// Whether the space `id` watches an open PR or has a request waiting for
/// a decision, its own or one it decides.
fn watches_or_asks(db: &Connection, id: &SpaceId) -> Result<bool> {
    if watches_open_pr(db, id)? {
        return Ok(true);
    }
    Ok(crate::daemon::approvals::pending_count(db, id)? > 0)
}

#[cfg(test)]
mod tests;
