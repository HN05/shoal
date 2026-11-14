//! The slots in use, counted from what the daemon already tracks, so a limit
//! applies however an agent started.
use crate::client::execution_ids::ExecutionId;
use crate::client::repository_ids::RepositoryId;
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::params;

use super::weight::Light;
use crate::client::space_ids::SpaceId;
use crate::client::time::UnixTime;
use crate::{
    config::modes::ModeName,
    daemon::workspace::{Manager, capacity::Activity},
    state::{ExecutionState, QueueKind, QueueState},
};

/// One used slot.
pub(crate) struct Slot {
    /// The repository of its space; none for a chat without one, whose
    /// slot counts toward the machine's limit alone.
    pub repository: Option<RepositoryId>,
    pub activity: Activity,
}

/// What the store records toward used slots, read in one pass.
struct PersistedSlots {
    /// Each space that is not done, by ID, with its repository ID; none
    /// for a chat without one.
    open: HashMap<SpaceId, Option<RepositoryId>>,
    /// Executions whose state is unknown, other than CI jobs' runners:
    /// agents that may still run.
    unknown: Vec<UnknownExecution>,
    /// Queue entries that are starting.
    starting: Vec<StartingEntry>,
}

/// An execution in the unknown state.
struct UnknownExecution {
    id: ExecutionId,
    space: SpaceId,
}

/// A starting queue entry.
struct StartingEntry {
    /// Its space's repository; none for a chat without one.
    repository: Option<RepositoryId>,
    /// The space it starts in, once known.
    space: Option<SpaceId>,
    /// When its holder was last seen.
    seen_at: Option<UnixTime>,
    /// The execution a restore resumes; a launch has none.
    execution: Option<ExecutionId>,
    /// The mode a launch's agent starts in.
    mode: Option<ModeName>,
}

/// Read the spaces, executions and queue entries that slots are counted from.
fn persisted_slots(db: &rusqlite::Connection) -> Result<PersistedSlots> {
    let open = db
        .prepare(
            "SELECT id,repository_id FROM spaces s WHERE NOT EXISTS
                (SELECT 1 FROM space_completion c WHERE c.space_id=s.id)",
        )?
        .query_map([], |row| Ok((row.get("id")?, row.get("repository_id")?)))?
        .collect::<rusqlite::Result<_>>()?;
    let unknown = db
        .prepare(
            "SELECT id,space_id FROM executions e WHERE state=?1
             AND NOT EXISTS (SELECT 1 FROM ci_jobs j WHERE j.workspace_id=e.space_id)",
        )?
        .query_map([ExecutionState::Unknown], |row| {
            Ok(UnknownExecution {
                id: row.get("id")?,
                space: row.get("space_id")?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let starting = db
        .prepare(
            "SELECT repository_id,space_id,seen_at,execution_id,mode FROM agent_queue
             WHERE state=?1 AND kind!=?2",
        )?
        .query_map(params![QueueState::Starting, QueueKind::Job], |row| {
            Ok(StartingEntry {
                repository: row.get("repository_id")?,
                space: row.get("space_id")?,
                seen_at: row.get("seen_at")?,
                execution: row.get("execution_id")?,
                mode: row.get("mode")?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(PersistedSlots {
        open,
        unknown,
        starting,
    })
}

impl Manager {
    /// The used slots. Each agent in a space that is not done uses one
    /// while it runs, awaits reattachment or waits for resume, counted once by
    /// its execution, except the `dormant` ones whose restore waits for an
    /// update; so does each starting launch until its agent registers. An
    /// admitted restore starts in its agent's slot, and one of a last
    /// session, which holds none, as a launch. A light agent holds none, nor
    /// does a one-shot run such as a review.
    pub(super) async fn occupied_slots(
        &self,
        dormant: &HashSet<ExecutionId>,
        light: &Light,
    ) -> Result<Vec<Slot>> {
        let mut agents = self.agent_executions().await;
        let PersistedSlots {
            open,
            unknown,
            starting,
        } = self.store.run(|db| persisted_slots(db)).await?;
        let now = UnixTime::now();
        let starting_since = |seen_at: Option<UnixTime>| {
            Activity::Starting(now.duration_since(seen_at.unwrap_or(now)))
        };
        let stopped = |space: SpaceId| (space, Activity::Stopped);
        for UnknownExecution { id, space } in unknown {
            agents.entry(id).or_insert_with(|| stopped(space));
        }
        for space in open.keys() {
            for execution in crate::execution::recovery::agent_records(&self.paths, space)? {
                agents
                    .entry(execution)
                    .or_insert_with(|| stopped(space.clone()));
            }
        }
        agents.retain(|execution, _| !dormant.contains(execution) && !light.execution(execution));
        let mut launches = Vec::new();
        for entry in starting {
            let repository = entry.repository.as_ref();
            if light.entry(entry.execution.as_ref(), repository, entry.mode.as_ref()) {
                continue;
            }
            match entry.execution.as_ref().and_then(|id| agents.get_mut(id)) {
                Some((_, activity)) => *activity = starting_since(entry.seen_at),
                None => launches.push(entry),
            }
        }
        let mut slots: Vec<Slot> = agents
            .into_values()
            .filter_map(|(space, activity)| {
                Some(Slot {
                    repository: open.get(&space)?.clone(),
                    activity,
                })
            })
            .collect();
        for launch in launches {
            if (launch.space.as_ref()).is_none_or(|space| open.contains_key(space)) {
                slots.push(Slot {
                    repository: launch.repository,
                    activity: starting_since(launch.seen_at),
                });
            }
        }
        Ok(slots)
    }
}
