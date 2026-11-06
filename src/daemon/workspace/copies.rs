//! Copies: workspaces that start from another workspace's current files or
//! from one of its checkpoints, and record what they started with in a
//! starting checkpoint of their own.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::model::{CopiedFrom, Workspace, WorkspaceRef};

/// Fill in what the workspace was copied from, when it is a copy.
pub(super) fn load(db: &Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.copied_from = db
        .query_row(
            "SELECT source.id, source.name, source.branch, copy.copied_from_checkpoint,
                copy.starting_checkpoint, json_extract(start.record, '$.head'),
                json_extract(start.record, '$.commit'), copy.starting_index
             FROM workspaces copy
             JOIN checkpoints start
                ON start.workspace_id=copy.id AND start.number=copy.starting_checkpoint
             LEFT JOIN workspaces source ON source.id=copy.copied_from
             WHERE copy.id=?1",
            [&workspace.id],
            |row| {
                let workspace = match row.get::<_, Option<String>>(0)? {
                    Some(id) => Some(WorkspaceRef {
                        id,
                        name: row.get(1)?,
                        branch: row.get(2)?,
                    }),
                    None => None,
                };
                Ok(CopiedFrom {
                    workspace,
                    checkpoint: row.get::<_, Option<i64>>(3)?.map(|number| number as u64),
                    starting_checkpoint: row.get::<_, i64>(4)? as u64,
                    head: row.get(5)?,
                    commit: row.get(6)?,
                    index: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                })
            },
        )
        .optional()?;
    Ok(())
}
