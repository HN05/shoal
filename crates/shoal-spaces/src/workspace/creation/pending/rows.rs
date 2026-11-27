//! The columns of `workspaces` that keep a pending worktree's plan: its
//! JSON, and the fields of its source that queries read.
use anyhow::Result;
use rusqlite::{Connection, Row, params, types::Value as SqlValue};
use shoal_ids::space_ids::SpaceId;
use shoal_store::split::{self, Column};

use super::PendingWorktree;

/// Each column, with the field of the plan it keeps: the source's kind, and
/// the checkpoint it names, such as the removal checkpoint a restore
/// creates the worktree from.
const FIELDS: &[Column] = &[
    ("pending_source", &["source", "kind"]),
    ("pending_checkpoint", &["source", "checkpoint"]),
];

/// What a query selects to read a plan.
pub(super) const COLUMNS: &str = "pending_plan,pending_source,pending_checkpoint";

/// The values of the columns [`COLUMNS`] names, in its order; null for a
/// workspace whose worktree exists.
pub(in crate::workspace) fn values(plan: Option<&PendingWorktree>) -> Result<[SqlValue; 3]> {
    let Some(plan) = plan else {
        return Ok([SqlValue::Null, SqlValue::Null, SqlValue::Null]);
    };
    let (json, fields) = split::split(plan, FIELDS)?;
    let [source, checkpoint] = <[SqlValue; 2]>::try_from(fields)
        .unwrap_or_else(|_| unreachable!("a value for each field"));
    Ok([SqlValue::Text(json), source, checkpoint])
}

/// None for a workspace whose worktree exists.
pub(super) fn plan(row: &Row<'_>) -> rusqlite::Result<Option<PendingWorktree>> {
    split::decode_optional_row(row, "pending_plan", FIELDS)
}

/// None for a workspace whose worktree exists.
pub(super) fn read(db: &Connection, id: &SpaceId) -> Result<Option<PendingWorktree>> {
    Ok(db.query_row(
        &format!("SELECT {COLUMNS} FROM workspaces WHERE id=?1"),
        [id],
        plan,
    )?)
}

pub(super) fn write(db: &Connection, id: &SpaceId, plan: &PendingWorktree) -> Result<()> {
    let [json, source, checkpoint] = values(Some(plan))?;
    db.execute(
        "UPDATE workspaces SET pending_plan=?2,pending_source=?3,pending_checkpoint=?4
            WHERE id=?1",
        params![id, json, source, checkpoint],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
