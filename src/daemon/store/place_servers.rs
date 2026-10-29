//! Key the records of places Shoal created by the server that holds each,
//! which the host names in the record's data: Herdr numbers each session's
//! panes from the start.
use anyhow::Result;
use rusqlite::{Connection, params};

use crate::sessions::Origin;

pub(super) fn key_by_server(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE owned_places_by_server (
            host TEXT NOT NULL,
            server TEXT NOT NULL,
            place TEXT NOT NULL,
            workspace_id TEXT,
            origin TEXT NOT NULL,
            PRIMARY KEY (host, server, place)
        );",
    )?;
    let records = db
        .prepare("SELECT host,place,workspace_id,origin FROM owned_places ORDER BY rowid")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (host, place, workspace, origin) in records {
        let server = serde_json::from_str::<Origin>(&origin)?.server();
        db.execute(
            "INSERT INTO owned_places_by_server(host,server,place,workspace_id,origin)
             VALUES (?1,?2,?3,?4,?5)",
            params![host, server, place, workspace, origin],
        )?;
    }
    db.execute_batch(
        "DROP TABLE owned_places;
         ALTER TABLE owned_places_by_server RENAME TO owned_places;",
    )?;
    Ok(())
}
