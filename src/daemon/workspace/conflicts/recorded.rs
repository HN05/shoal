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
    let row = db
        .query_row(
            "SELECT target,target_commit,head,files FROM changed_on_both_sides
                WHERE workspace_id=?1",
            [workspace_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    row.map(|(target, target_commit, head, files)| {
        Ok(ChangedOnBothSides {
            target,
            target_commit,
            head,
            files: serde_json::from_str(&files)?,
        })
    })
    .transpose()
}
