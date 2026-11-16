//! The recorded Compose project of each workspace.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;

use shoal_protocol::containers::ContainerProject;

/// A project as recorded, with whether setup started it for the
/// workspace's own agent, which takes it over when it starts.
#[derive(Debug, Clone)]
pub(crate) struct Recorded {
    pub(crate) project: ContainerProject,
    pub(crate) for_agent: bool,
}

const COLUMNS: &str =
    "workspace_id,project,runtime,compose,state,acquirer,acquirer_execution,for_agent,reason,error";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Recorded> {
    Ok(Recorded {
        project: ContainerProject {
            workspace_id: row.get("workspace_id")?,
            project: row.get("project")?,
            runtime: row.get("runtime")?,
            compose: row.get("compose")?,
            state: row.get("state")?,
            acquirer: crate::daemon::resources::row_acquirer(row)?,
            reason: row.get("reason")?,
            error: row.get("error")?,
        },
        for_agent: row.get("for_agent")?,
    })
}

/// The project of the workspace `id`.
pub(crate) fn find(db: &Connection, id: &SpaceId) -> Result<Option<Recorded>> {
    let query = format!("SELECT {COLUMNS} FROM container_projects WHERE workspace_id=?1");
    Ok(db.query_row(&query, [id], row).optional()?)
}

/// The projects of the workspace `owner`, or of every workspace.
pub(crate) fn list(db: &Connection, owner: Option<&SpaceId>) -> Result<Vec<Recorded>> {
    let query = format!(
        "SELECT {COLUMNS} FROM container_projects WHERE ?1 IS NULL OR workspace_id=?1
            ORDER BY workspace_id"
    );
    let mut statement = db.prepare(&query)?;
    let rows = statement.query_map([owner], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Record `recorded`, replacing the workspace's earlier record.
pub(crate) fn save(db: &Connection, recorded: &Recorded) -> Result<()> {
    let project = &recorded.project;
    db.execute(
        &format!(
            "INSERT OR REPLACE INTO container_projects({COLUMNS})
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"
        ),
        params![
            project.workspace_id,
            project.project,
            project.runtime,
            project.compose,
            project.state,
            project.acquirer.kind(),
            project.acquirer.execution(),
            recorded.for_agent,
            project.reason,
            project.error,
        ],
    )?;
    Ok(())
}

/// Forget the project of the workspace `id`, once `compose down` removed it.
pub(crate) fn delete(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute("DELETE FROM container_projects WHERE workspace_id=?1", [id])?;
    Ok(())
}
