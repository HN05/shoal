//! The preparation of migration 339. It moves the host, ID and instance of
//! each place that executions and owned places record out of the place's
//! JSON into the columns migration 338 added. An owned place's columns held
//! its host and ID already, which its JSON must match.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

pub(super) fn move_into_columns(db: &Connection) -> Result<()> {
    move_execution_places(db)?;
    move_owned_places(db)
}

fn move_execution_places(db: &Connection) -> Result<()> {
    for (id, origin) in origins(
        db,
        "SELECT id,origin FROM executions WHERE origin IS NOT NULL",
    )? {
        let moved = split(&origin).with_context(|| format!("executions row {id}"))?;
        db.execute(
            "UPDATE executions SET origin_host=?2,origin_place=?3,origin_instance=?4,origin=?5
             WHERE id=?1",
            params![id, moved.host, moved.place, moved.instance, moved.rest],
        )?;
    }
    Ok(())
}

fn move_owned_places(db: &Connection) -> Result<()> {
    let rows = {
        let mut reading =
            db.prepare("SELECT rowid,origin_host,origin_place,origin FROM owned_places")?;
        let rows = reading.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<(i64, String, String, String)>>>()?
    };
    for (rowid, host, place, origin) in rows {
        let moved = split(&origin).with_context(|| format!("owned_places row {rowid}"))?;
        ensure!(
            moved.host == host && moved.place.as_deref() == Some(place.as_str()),
            "owned_places row {rowid}: its origin names {} place {:?}, not its {host} place {place}",
            moved.host,
            moved.place
        );
        db.execute(
            "UPDATE owned_places SET origin_instance=?2,origin=?3 WHERE rowid=?1",
            params![rowid, moved.instance, moved.rest],
        )?;
    }
    Ok(())
}

/// The ID and origin of each row `query` selects.
fn origins(db: &Connection, query: &str) -> Result<Vec<(String, String)>> {
    let mut reading = db.prepare(query)?;
    let rows = reading.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A place's host, ID and instance, and the JSON of the rest of it.
struct Moved {
    host: String,
    place: Option<String>,
    instance: Option<String>,
    rest: String,
}

fn split(origin: &str) -> Result<Moved> {
    let Value::Object(mut origin) = serde_json::from_str(origin).context("read its place")? else {
        bail!("its place is not an object");
    };
    let host = match origin.remove("host") {
        Some(Value::String(host)) => host,
        _ => bail!("its place names no host"),
    };
    Ok(Moved {
        host,
        place: optional_text(&mut origin, "place")?,
        instance: optional_text(&mut origin, "instance")?,
        rest: Value::Object(origin).to_string(),
    })
}

/// The text of `origin`'s optional `field`, which leaves it.
fn optional_text(origin: &mut Map<String, Value>, field: &str) -> Result<Option<String>> {
    match origin.remove(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => bail!("its place's {field} is not text"),
    }
}
