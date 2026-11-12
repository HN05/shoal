//! The spaces as ranking sees them: each one's priority and own tier, its
//! plan steps and when it was created.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::super::rank::{Space, Steps};
use crate::client::{events::EventKind, priority::Priority, space_ids::SpaceId, time::UnixTime};
use crate::{
    daemon::{progress, store},
    forge::{items::link::LinkKind, updates::SnapshotState},
    model::Tier,
};

/// A space as the store records it.
struct Row {
    id: SpaceId,
    number: Option<i64>,
    name: String,
    priority: Option<Priority>,
    /// Whether it has a ready mark.
    ready: bool,
    /// Whether an agent has worked in it.
    started: bool,
    created_at: Option<UnixTime>,
}

/// Every space, by ID.
pub(super) fn ranked(db: &Connection) -> Result<HashMap<SpaceId, Space>> {
    let mut spaces = HashMap::new();
    for row in rows(db)? {
        let space = space(db, &row)?;
        spaces.insert(row.id, space);
    }
    Ok(spaces)
}

fn rows(db: &Connection) -> Result<Vec<Row>> {
    Ok(db
        .prepare(
            "SELECT s.id,s.number,s.name,
                (SELECT priority FROM space_priority WHERE space_id=s.id),
                EXISTS(SELECT 1 FROM space_review WHERE space_id=s.id),
                s.kind='chat' OR EXISTS(SELECT 1 FROM space_agent_state WHERE space_id=s.id)
                    OR EXISTS(SELECT 1 FROM workspace_events WHERE space_id=s.id AND NOT bulk
                        AND json_extract(record,'$.kind')=?1),
                (SELECT created_at FROM workspace_events WHERE space_id=s.id AND NOT bulk
                    ORDER BY id LIMIT 1)
             FROM spaces s",
        )?
        .query_map(params![EventKind::AgentStarted], |row| {
            Ok(Row {
                id: row.get(0)?,
                number: row.get(1)?,
                name: row.get(2)?,
                priority: row.get(3)?,
                ready: row.get(4)?,
                started: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// The space `row` records, with its open PRs and plan steps.
fn space(db: &Connection, row: &Row) -> Result<Space> {
    let tier = if row.ready || has_open_pr(db, &row.id)? {
        Tier::Finishing
    } else if row.started {
        Tier::Started
    } else {
        Tier::New
    };
    let steps = progress::current(db, &row.id)?.map(|progress| Steps {
        done: progress.done(),
        total: progress.steps.len(),
    });
    Ok(Space {
        label: row
            .number
            .map_or(row.name.clone(), |number| number.to_string()),
        priority: row.priority,
        tier,
        steps: steps.unwrap_or_default(),
        created_at: row.created_at.unwrap_or(UnixTime::from_seconds(0)),
    })
}

/// Whether the space `id` has a PR it opened or was assigned that is open,
/// or that no watch has read yet.
fn has_open_pr(db: &Connection, id: &SpaceId) -> Result<bool> {
    let links = store::space_links(db, id)?;
    let opened = (links.opened.iter())
        .filter(|item| item.kind() == LinkKind::Pr)
        .map(|item| &item.url);
    for url in links.prs.iter().chain(opened) {
        // The background poll's observation is current; a watch's cursor
        // waits for its deliveries to be read.
        let state: Option<Option<String>> = db
            .query_row(
                "SELECT COALESCE(
                    (SELECT json_extract(record,'$.state') FROM space_item_activity
                        WHERE space_id=?1 AND url=?2),
                    (SELECT json_extract(record,'$.state') FROM pr_activity
                        WHERE space_id=?1 AND url=?2))",
                params![id, url],
                |row| row.get(0),
            )
            .optional()?;
        let ended = [SnapshotState::Merged, SnapshotState::Closed]
            .map(SnapshotState::as_str)
            .map(Some);
        if !ended.contains(&state.flatten().as_deref()) {
            return Ok(true);
        }
    }
    Ok(false)
}
