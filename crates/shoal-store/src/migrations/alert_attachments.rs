//! The preparation of migration 316. It moves the paths each alert's
//! `attachments` array held into `alert_attachments`, in their order.
use anyhow::{Context as _, Result};
use rusqlite::{Connection, params};

pub(super) fn move_paths(db: &Connection) -> Result<()> {
    let alerts = db
        .prepare("SELECT id,attachments FROM alerts ORDER BY id")?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut insert =
        db.prepare("INSERT INTO alert_attachments(alert_id,position,path) VALUES (?1,?2,?3)")?;
    for (id, attachments) in alerts {
        let paths: Vec<String> = serde_json::from_str(&attachments)
            .with_context(|| format!("alerts row {id} attaches no list of paths"))?;
        for (position, path) in (0_i64..).zip(&paths) {
            insert.execute(params![id, position, path])?;
        }
    }
    Ok(())
}
