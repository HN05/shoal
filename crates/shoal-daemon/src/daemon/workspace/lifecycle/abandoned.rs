//! Removals that stop before they finish without retaining their
//! workspace, as when the daemon stops: the workspace leaves `removing` for
//! `failed`, and its log says where its removal stopped, so `shoal rm`
//! finishes it.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use crate::client::space_ids::{SpaceId, SpaceNumber};
use crate::{
    daemon::events::{self, EventCause, EventKind},
    state::WorkspaceState,
};

/// Fail every workspace a removal left in `removing` when the daemon
/// stopped, in the transaction that quarantines the other interrupted
/// operations at startup.
pub(crate) fn fail_interrupted_removals(tx: &Connection) -> Result<()> {
    let ids = tx
        .prepare("SELECT id FROM workspaces WHERE state=?1")?
        .query_map([WorkspaceState::Removing], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<SpaceId>>>()?;
    for id in ids {
        fail_stopped_removal(tx, &id, "the daemon stopped")?;
    }
    Ok(())
}

/// Fail the workspace `id` while a removal that stopped because `why` left
/// it in `removing`, recording that it stays, for that removal's cause.
fn fail_stopped_removal(tx: &Connection, id: &SpaceId, why: &str) -> Result<()> {
    let number: Option<Option<SpaceNumber>> = tx
        .query_row(
            "SELECT number FROM workspaces WHERE id=?1 AND state=?2",
            params![id, WorkspaceState::Removing],
            |row| row.get(0),
        )
        .optional()?;
    let Some(number) = number else {
        return Ok(());
    };
    let space = number.map_or_else(|| id.to_string(), |number| number.to_string());
    let error = format!("removal stopped when {why}; shoal rm {space} finishes it");
    let cause = started_cause(tx, id)?;
    events::retain(tx, id, WorkspaceState::Failed, cause, &error)
}

/// The cause of the latest removal of the workspace `id` that its log
/// records as started.
fn started_cause(tx: &Connection, id: &SpaceId) -> Result<EventCause> {
    let cause = tx
        .query_row(
            "SELECT json_extract(record,'$.cause') FROM workspace_events
             WHERE space_id=?1 AND json_extract(record,'$.kind')=?2
             ORDER BY id DESC LIMIT 1",
            params![id, EventKind::RemovalStarted],
            |row| row.get::<_, Option<EventCause>>(0),
        )
        .optional()?
        .flatten();
    Ok(cause.unwrap_or(EventCause::Manual))
}

#[cfg(test)]
mod tests;
