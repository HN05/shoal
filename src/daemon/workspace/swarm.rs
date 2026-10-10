//! Swarms: workspaces of one repository that attempt the same task.
use anyhow::Result;
use rusqlite::{Connection, params};

use super::stack;
use crate::model::{Swarm, Workspace};

/// Fill in the workspace's swarm with the swarm's other workspaces.
pub(super) fn load(db: &Connection, workspace: &mut Workspace) -> Result<()> {
    let task: Option<String> = db.query_row(
        "SELECT swarm FROM workspaces WHERE id=?1",
        [&workspace.id],
        |row| row.get(0),
    )?;
    workspace.swarm = match task {
        Some(task) => Some(Swarm {
            workspaces: db
                .prepare(
                    "SELECT id,name,branch FROM workspaces
                     WHERE repository_id=?1 AND swarm=?2 AND id<>?3 ORDER BY name",
                )?
                .query_map(
                    params![workspace.repository_id, task, workspace.id],
                    stack::row,
                )?
                .collect::<rusqlite::Result<_>>()?,
            task,
        }),
        None => None,
    };
    Ok(())
}

#[cfg(test)]
mod tests;
