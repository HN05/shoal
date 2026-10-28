//! Durable terminal cleanup requests, committed before a demo record disappears.
use anyhow::{Context, Result};
use rusqlite::{Connection, Transaction, params};

use crate::sessions::Owner;

pub(super) struct Close {
    pub id: String,
    pub host: String,
    pub name: String,
    pub owner: Owner,
}

/// Queue only hosts that supply session cleanup. The unique launch ID binds
/// retries to this demo even if its host has since reused the session's name.
pub(in crate::daemon::workspace) fn record_demo(
    tx: &Transaction<'_>,
    execution: &str,
    hosts: &[&str],
) -> Result<Option<String>> {
    let Some(launch) = super::super::demos::execution_launch(tx, execution)? else {
        return Ok(None);
    };
    if !hosts.contains(&launch.origin.host.as_str()) {
        return Ok(None);
    }
    tx.execute(
        "INSERT OR IGNORE INTO session_cleanup(id,host,name,owner) VALUES (?1,?2,?3,?4)",
        params![
            launch.id,
            launch.origin.host,
            launch.origin.id(),
            serde_json::to_string(&Owner::Demo(launch.id.clone()))?
        ],
    )?;
    Ok(Some(launch.id))
}

pub(super) fn list(db: &Connection, id: Option<&str>) -> Result<Vec<Close>> {
    let mut statement = db.prepare(
        "SELECT id,host,name,owner FROM session_cleanup WHERE ?1 IS NULL OR id=?1 ORDER BY id",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (id, host, name, owner) = row?;
        Ok(Close {
            id,
            host,
            name,
            owner: serde_json::from_str(&owner).context("invalid session cleanup owner")?,
        })
    })
    .collect()
}

pub(super) fn forget(db: &Connection, id: &str) -> Result<()> {
    db.execute("DELETE FROM session_cleanup WHERE id=?1", [id])?;
    Ok(())
}
