//! Give every record of a place Shoal created a space as its owner, a
//! workspace or a chat, whose removal closes the place. Records a chat took
//! over had no owner: each goes to the chat recorded running there, and one
//! whose chat is gone, which nothing would close, is forgotten.
use crate::sessions::OriginExt;
use anyhow::Result;
use rusqlite::{Connection, params};

use crate::sessions::Origin;

pub(super) fn own_by_space(db: &Connection) -> Result<()> {
    // A migration from an earlier version applies again to records that
    // name their space already.
    let owned_by_space = crate::daemon::store::exists(
        db,
        "SELECT 1 FROM pragma_table_info('owned_places') WHERE name='space_id'",
        [],
    )?;
    if owned_by_space {
        return Ok(());
    }
    db.execute_batch(
        "CREATE TABLE owned_places_by_space (
            host TEXT NOT NULL,
            server TEXT NOT NULL,
            place TEXT NOT NULL,
            space_id TEXT NOT NULL,
            origin TEXT NOT NULL,
            PRIMARY KEY (host, server, place)
        );",
    )?;
    let chats = chat_places(db)?;
    let records = db
        .prepare("SELECT host,server,place,workspace_id,origin FROM owned_places ORDER BY rowid")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (host, server, place, workspace, origin) in records {
        let recorded = serde_json::from_str::<Origin>(&origin)?;
        let owner = workspace.or_else(|| {
            chats
                .iter()
                .find(|(_, at)| same_place(at, &recorded))
                .map(|(chat, _)| chat.clone())
        });
        let Some(owner) = owner else {
            continue;
        };
        db.execute(
            "INSERT INTO owned_places_by_space(host,server,place,space_id,origin)
             VALUES (?1,?2,?3,?4,?5)",
            params![host, server, place, owner, origin],
        )?;
    }
    db.execute_batch(
        "DROP TABLE owned_places;
         ALTER TABLE owned_places_by_space RENAME TO owned_places;",
    )?;
    Ok(())
}

/// Each chat with the place it runs in.
fn chat_places(db: &Connection) -> Result<Vec<(String, Origin)>> {
    let rows = db
        .prepare("SELECT id,origin FROM chats WHERE origin IS NOT NULL ORDER BY rowid")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(chat, origin)| Some((chat, serde_json::from_str(&origin).ok()?)))
        .collect())
}

/// Whether `a` and `b` name one instance of a place on one server.
fn same_place(a: &Origin, b: &Origin) -> bool {
    a.host == b.host
        && a.place.is_some()
        && a.place == b.place
        && a.server() == b.server()
        && a.instance == b.instance
}
