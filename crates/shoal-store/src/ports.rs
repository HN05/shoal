//! The TCP port reservations workspaces hold, as their rows record them.
use anyhow::Result;
use rusqlite::{Connection, Row};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::PortReservation;

const PORT_COLUMNS: &str = "workspace_id,name,port,env_var,reason";

fn port(row: &Row<'_>) -> rusqlite::Result<PortReservation> {
    Ok(PortReservation {
        workspace_id: row.get("workspace_id")?,
        name: row.get("name")?,
        port: row.get("port")?,
        env_var: row.get("env_var")?,
        reason: row.get("reason")?,
    })
}

fn ports_query(by_owner: bool) -> String {
    let filter = if by_owner {
        "WHERE workspace_id=?1"
    } else {
        ""
    };
    format!("SELECT {PORT_COLUMNS} FROM ports {filter} ORDER BY workspace_id,name")
}

pub fn ports(db: &Connection, workspace_id: Option<&SpaceId>) -> Result<Vec<PortReservation>> {
    Ok(db
        .prepare(&ports_query(workspace_id.is_some()))?
        .query_map(rusqlite::params_from_iter(workspace_id), port)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests;
