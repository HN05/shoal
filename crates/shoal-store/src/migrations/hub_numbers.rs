//! The preparation of migration 318. It moves the ID and number out of
//! each space summary the hub keeps into the columns beside it, and each
//! numbering's spares into a table of their own.
use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

/// Each table of summaries, the table that takes its rows, and the columns
/// both have besides the space, its machine, its number and its summary.
const SUMMARIES: &[(&str, &str, &str)] = &[
    ("hub_spaces", "hub_spaces_numbered", "reported_at"),
    (
        "hub_space_copies",
        "hub_space_copies_numbered",
        "since,handed_over",
    ),
];

/// Each numbering and the table that takes its spares.
const SPARES: &[(&str, &str)] = &[
    ("hub_numbering", "hub_numbering_spares"),
    ("hub_alert_numbering", "hub_alert_numbering_spares"),
];

pub(super) fn move_numbers(db: &Connection) -> Result<()> {
    for (table, numbered, others) in SUMMARIES {
        move_summaries(db, table, numbered, others)?;
    }
    for (table, spares) in SPARES {
        move_spares(db, table, spares)?;
    }
    Ok(())
}

fn move_summaries(db: &Connection, table: &str, numbered: &str, others: &str) -> Result<()> {
    let rows = db
        .prepare(&format!("SELECT rowid,space_id,summary FROM {table}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut insert = db.prepare(&format!(
        "INSERT INTO {numbered}(space_id,member_id,number,summary,{others})
            SELECT space_id,member_id,?2,?3,{others} FROM {table} WHERE rowid=?1"
    ))?;
    for (rowid, space, summary) in rows {
        let (number, rest) = without_id_and_number(&space, &summary)
            .with_context(|| format!("{table} row {rowid}, of space {space}"))?;
        insert.execute(params![rowid, number, rest])?;
    }
    Ok(())
}

/// The number in the summary `text` of the space `space`, and the summary
/// without its ID and number.
fn without_id_and_number(space: &str, text: &str) -> Result<(i64, String)> {
    let Value::Object(mut summary) = serde_json::from_str::<Value>(text)? else {
        bail!("the summary is no JSON object");
    };
    let id = summary.remove("id");
    ensure!(
        id.as_ref().and_then(Value::as_str) == Some(space),
        "the summary names space {id:?}"
    );
    let Some(number) = summary.remove("number").as_ref().and_then(Value::as_i64) else {
        bail!("the summary has no number");
    };
    Ok((number, serde_json::to_string(&Map::from_iter(summary))?))
}

fn move_spares(db: &Connection, table: &str, spares: &str) -> Result<()> {
    let rows = db
        .prepare(&format!("SELECT id,spares FROM {table}"))?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // A number listed twice is one spare.
    let mut insert = db.prepare(&format!(
        "INSERT OR IGNORE INTO {spares}(number) VALUES (?1)"
    ))?;
    for (id, text) in rows {
        let numbers: Vec<i64> = serde_json::from_str(&text)
            .with_context(|| format!("{table} row {id} keeps no list of spare numbers"))?;
        for number in numbers {
            insert.execute([number])?;
        }
    }
    Ok(())
}
