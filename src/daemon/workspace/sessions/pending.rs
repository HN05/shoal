//! Durable terminal cleanup requests, committed before a demo record disappears.
use anyhow::{Context, Result};
use rusqlite::{Connection, Transaction, params};

use crate::sessions::{Origin, Owner};

pub(super) struct Close {
    pub id: String,
    pub place: Origin,
    pub owner: Owner,
}

/// Queue only hosts that supply session cleanup. The unique launch ID binds
/// retries to this demo even if its host has since reused the session's name.
pub(in crate::daemon::workspace) fn record_demo(
    tx: &Transaction<'_>,
    execution: &str,
    hosts: &[crate::sessions::HostName],
) -> Result<Option<String>> {
    let Some(launch) = super::super::demos::execution_launch(tx, execution)? else {
        return Ok(None);
    };
    if !hosts.contains(&launch.origin.host) {
        return Ok(None);
    }
    tx.execute(
        "INSERT OR IGNORE INTO session_cleanup(id,host,name,owner,origin) VALUES (?1,?2,?3,?4,?5)",
        params![
            launch.id,
            launch.origin.host,
            launch.origin.id(),
            serde_json::to_string(&Owner::Demo(launch.id.clone()))?,
            serde_json::to_string(&launch.origin)?
        ],
    )?;
    Ok(Some(launch.id))
}

pub(super) fn list(db: &Connection, id: Option<&str>) -> Result<Vec<Close>> {
    let mut statement = db.prepare(
        "SELECT id,host,name,owner,origin FROM session_cleanup WHERE ?1 IS NULL OR id=?1 ORDER BY id",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok((
            row.get(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    rows.map(|row| {
        let (id, host, name, owner, origin) = row?;
        // Closes queued before origins name only their host and session.
        let place = match origin {
            Some(origin) => {
                serde_json::from_str(&origin).context("invalid session cleanup place")?
            }
            None => Origin::created(host.parse()?, name),
        };
        Ok(Close {
            id,
            place,
            owner: serde_json::from_str(&owner).context("invalid session cleanup owner")?,
        })
    })
    .collect()
}

pub(super) fn forget(db: &Connection, id: &str) -> Result<()> {
    db.execute("DELETE FROM session_cleanup WHERE id=?1", [id])?;
    Ok(())
}
