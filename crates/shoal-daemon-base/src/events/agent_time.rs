//! How long the agents of each repository ran, by the journal's starts and
//! ends of each space's agent.
use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    time::Duration,
};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::config::modes::ModeName;

use super::{EventItem, EventKind, STORED, Stored};
use crate::time::UnixTime;

/// How long the agents of a repository's spaces ran in one mode, none
/// for those whose events record none.
pub type AgentTime = HashMap<(RepositoryId, Option<ModeName>), Duration>;

/// The condition that selects agent events. Its kinds are literals, so a
/// query reads the index of agent events.
const AGENT_EVENTS: &str = "kind IN ('agent_started','agent_stopped','agent_exited')";

/// An agent event of a space in a repository.
struct Row {
    space: SpaceId,
    repository: RepositoryId,
    started: bool,
    at: UnixTime,
    execution: Option<ExecutionId>,
    mode: Option<ModeName>,
}

/// How long the agents of each repository's spaces ran from `since` until
/// `now`, by mode. A start runs until the end of its execution's agent,
/// or until `now` without one, and counts from `since` when it came
/// before. An end counts only after its execution's start, so a launch
/// that failed before its agent started counts nothing. An agent that ran
/// through all of the time read records no event there and counts
/// nothing, which Shoal's stops of idle agents keep rare.
pub fn running_since(db: &Connection, since: UnixTime, now: UnixTime) -> Result<AgentTime> {
    let mut running: HashMap<SpaceId, Row> = HashMap::new();
    let mut read = HashSet::new();
    let mut time = AgentTime::new();
    let mut add = |start: Row, to: UnixTime| {
        let from = start.at.max(since);
        *time.entry((start.repository, start.mode)).or_default() += to.duration_since(from);
    };
    for row in rows(db, since)? {
        if read.insert(row.space.clone())
            && !row.started
            && let Some(start) = started_before(db, &row.space, since)?
        {
            running.insert(row.space.clone(), start);
        }
        if row.started {
            running.insert(row.space.clone(), row);
        } else if let Entry::Occupied(start) = running.entry(row.space.clone())
            && start.get().execution == row.execution
        {
            add(start.remove(), row.at);
        }
    }
    for start in running.into_values() {
        add(start, now);
    }
    Ok(time)
}

/// The agent events of spaces in a repository since `since`, oldest first.
fn rows(db: &Connection, since: UnixTime) -> Result<Vec<Row>> {
    let mut statement = db.prepare(&format!(
        "SELECT {STORED} FROM space_events WHERE {AGENT_EVENTS} AND created_at>=?1 ORDER BY id"
    ))?;
    let stored = statement.query_map(params![since], Stored::read)?;
    let mut rows = Vec::new();
    for stored in stored {
        rows.extend(row(stored?.decode()?));
    }
    Ok(rows)
}

/// The start of the agent the space `space` ran as `since` began, none
/// when its newest agent event before then is no start.
fn started_before(db: &Connection, space: &SpaceId, since: UnixTime) -> Result<Option<Row>> {
    let newest = db
        .query_row(
            &format!(
                "SELECT {STORED} FROM space_events WHERE space_id=?1 AND {AGENT_EVENTS}
                    AND created_at<?2 ORDER BY id DESC LIMIT 1"
            ),
            params![space, since],
            Stored::read,
        )
        .optional()?;
    let row = match newest {
        Some(stored) => row(stored.decode()?),
        None => None,
    };
    Ok(row.filter(|row| row.started))
}

/// The agent event `item` holds, none for another event or one of a space
/// in no repository.
fn row(item: EventItem) -> Option<Row> {
    let EventItem::Event(event) = item else {
        return None;
    };
    let details = event.details;
    let agent = details.event.agent()?;
    Some(Row {
        space: details.space_id.clone(),
        repository: details.repository_id.clone()?,
        started: details.kind() == EventKind::AgentStarted,
        at: event.created_at,
        execution: agent.execution_id.clone(),
        mode: agent.mode.clone(),
    })
}

#[cfg(test)]
mod tests;
