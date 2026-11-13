//! Put idle agents to sleep to make room for queued work: while an entry
//! waits for a slot or memory, an idle agent that something will wake and
//! whose priority is no higher than the entry's goes to sleep, lowest rank
//! first, once that costs nothing. One that nothing would wake stays
//! running, as do light agents and those the user attends. A woken agent
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

/// A queued entry waiting for room, with its priority and, while its
/// repository's limit holds it, that repository.
struct Waiting {
    priority: Priority,
    repository: Option<RepositoryId>,
}

/// An idle agent that may sleep to make room.
struct Sleeper<'a> {
    candidate: &'a Candidate,
    /// Its space's repository; none for a chat without one.
    repository: Option<RepositoryId>,
    priority: Priority,
    free: Free,
}

impl Sleeper<'_> {
    /// Whether sleeping makes room for `entry`: its priority is no higher,
    /// and it frees a slot of the entry's repository when that is what the
    /// entry waits for.
    fn serves(&self, entry: &Waiting) -> bool {
        self.priority <= entry.priority
            && (entry.repository.as_ref())
                .is_none_or(|repository| self.repository.as_ref() == Some(repository))
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
                let repository = match entry.held_by? {
                    HeldBy::RepositoryLimit => Some(entry.repository_id?),
                    HeldBy::MaxAgents | HeldBy::Memory { .. } | HeldBy::MemoryKept => None,
                    _ => return None,
                };
                let priority = entry.rank.map(|rank| rank.priority).unwrap_or_default();
                Some(Waiting {
                    priority,
                    repository,
                })
            })
            .collect())
    }

    /// The `candidates` not already `chosen` that may sleep for room at
    /// `now`, lowest rank first: heavy agents the user does not attend,
    /// which something will wake and for which sleeping costs nothing.
    async fn sleepers<'a>(
        &self,
        candidates: &'a [Candidate],
        chosen: &HashMap<ExecutionId, Chosen>,
        now: UnixTime,
    ) -> Result<Vec<Sleeper<'a>>> {
        let light = self.light().await?;
        let mut eligible = Vec::new();
        for candidate in candidates {
            let (execution, id) = (&candidate.execution, &candidate.workspace_id);
            if chosen.contains_key(execution) || candidate.attended || light.execution(execution) {
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
/// a decision, its own or, for a chat, one it decides.
fn watches_or_asks(db: &Connection, id: &SpaceId) -> Result<bool> {
    if watches_open_pr(db, id)? {
        return Ok(true);
    }
    let chat = crate::daemon::links::space::is_chat(db, id)?;
    Ok(crate::daemon::approvals::pending_count(db, id, chat)? > 0)
}
