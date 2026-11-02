//! The last check of each workspace against its default conflict target,
//! which merging reads instead of checking again.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{ChangedOnBothSides, ConflictCheck};

/// Keep `check` as the workspace's latest, unless the workspace is gone.
pub(super) fn save(db: &Connection, check: &ConflictCheck) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO changed_on_both_sides(workspace_id,target,target_commit,head,files)
            SELECT ?1,?2,?3,?4,?5 WHERE EXISTS (SELECT 1 FROM workspaces WHERE id=?1)",
        params![
            check.workspace_id,
            check.target,
            check.target_commit,
            check.head,
            serde_json::to_string(&check.changed_on_both_sides)?,
        ],
    )?;
    Ok(())
}

pub(crate) fn get(db: &Connection, workspace_id: &str) -> Result<Option<ChangedOnBothSides>> {
    db.query_row(
        &format!("SELECT {COLUMNS} FROM changed_on_both_sides WHERE workspace_id=?1"),
        [workspace_id],
        |row| Ok(Row::read(row)),
    )
    .optional()?
    .map(|row| Ok(row?.record()?.1))
    .transpose()
}

/// Each workspace's record that lists files, by workspace ID.
pub(crate) fn listing_files(db: &Connection) -> Result<Vec<(String, ChangedOnBothSides)>> {
    let mut statement = db.prepare(&format!(
        "SELECT {COLUMNS} FROM changed_on_both_sides WHERE files != '[]'"
    ))?;
    let rows = statement.query_map([], |row| Ok(Row::read(row)))?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row??.record()?);
    }
    Ok(records)
}

const COLUMNS: &str = "workspace_id,target,target_commit,head,files";

/// A row of [`COLUMNS`], its files still JSON.
struct Row {
    workspace_id: String,
    target: String,
    target_commit: String,
    head: String,
    files: String,
}

impl Row {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            workspace_id: row.get(0)?,
            target: row.get(1)?,
            target_commit: row.get(2)?,
            head: row.get(3)?,
            files: row.get(4)?,
        })
    }

    fn record(self) -> Result<(String, ChangedOnBothSides)> {
        let record = ChangedOnBothSides {
            target: self.target,
            target_commit: self.target_commit,
            head: self.head,
            files: serde_json::from_str(&self.files)?,
        };
        Ok((self.workspace_id, record))
    }
}
