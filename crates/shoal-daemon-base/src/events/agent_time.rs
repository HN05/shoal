//! How long the agents of each repository ran, by the journal's starts and
//! ends of each space's agent.
use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::config::modes::ModeName;

use super::EventKind;
use crate::time::UnixTime;

/// How long the agents of a repository's spaces ran in one mode, none
/// for those whose events do not record it.
pub type AgentTime = HashMap<(RepositoryId, Option<ModeName>), Duration>;

/// An agent event of a space in a repository.
struct Row {
    space: SpaceId,
    repository: RepositoryId,
    started: bool,
    at: UnixTime,
    mode: Option<ModeName>,
}

/// What a space's agent last did in the time read.
enum Last<'a> {
    /// It started, by this event.
    Started(&'a Row),
    Ended,
}

/// How long the agents of each repository's spaces ran from `since` until
/// `now`, by mode. A space runs one agent at a time, so a start runs until
/// the space's next end. An end with no start before it in that time ran
/// from `since`. An agent that ran through all of it records no event
/// there and counts nothing, which Shoal's stops of idle agents keep rare.
pub fn running_since(db: &Connection, since: UnixTime, now: UnixTime) -> Result<AgentTime> {
    let rows = rows(db, since)?;
    let mut last: HashMap<&SpaceId, Last> = HashMap::new();
    let mut time = AgentTime::new();
    let mut add = |row: &Row, from: UnixTime, to: UnixTime| {
        let key = (row.repository.clone(), row.mode.clone());
        *time.entry(key).or_default() += to.duration_since(from);
    };
    for row in &rows {
        if row.started {
            last.insert(&row.space, Last::Started(row));
            continue;
        }
        match last.insert(&row.space, Last::Ended) {
            Some(Last::Started(start)) => add(start, start.at, row.at),
            None => add(row, since, row.at),
            Some(Last::Ended) => {}
        }
    }
    for start in last.into_values() {
        if let Last::Started(start) = start {
            add(start, start.at, now);
        }
    }
    Ok(time)
}

/// The agent events of spaces in a repository since `since`, oldest first.
fn rows(db: &Connection, since: UnixTime) -> Result<Vec<Row>> {
    // The kinds are literals, so the query reads the index of agent events.
    let sql = format!(
        "SELECT space_id,repository_id,kind,created_at,json_extract(record,'$.agent.mode')
         FROM space_events
         WHERE kind IN ('{}','{}','{}') AND created_at>=?1
             AND space_id IS NOT NULL AND repository_id IS NOT NULL
         ORDER BY id",
        EventKind::AgentStarted,
        EventKind::AgentStopped,
        EventKind::AgentExited,
    );
    let mut statement = db.prepare(&sql)?;
    let rows = statement.query_map(params![since], |row| {
        Ok(Row {
            space: row.get(0)?,
            repository: row.get(1)?,
            started: row.get::<_, String>(2)? == EventKind::AgentStarted.as_str(),
            at: row.get(3)?,
            mode: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
