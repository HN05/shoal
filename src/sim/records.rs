//! Indexed ownership lookup over the authoritative JSON records.
use anyhow::{Context, Result, ensure};
use rusqlite::Connection;

use super::Simulator;

const ALL: &str = "SELECT id,record FROM simulators ORDER BY id";
// Keep the expression identical to the schema index. An optional-parameter OR
// would prevent SQLite from seeking directly to the owner's records.
const OWNED: &str = "SELECT id,record FROM simulators
    WHERE coalesce(json_extract(record, '$.workspace_id'), json_extract(record, '$.last_workspace_id')) = ?1
    ORDER BY id";

pub(crate) fn list(db: &Connection, owner: Option<&str>) -> Result<Vec<Simulator>> {
    let mut statement = db.prepare(if owner.is_some() { OWNED } else { ALL })?;
    let mut rows = match owner {
        Some(owner) => statement.query([owner])?,
        None => statement.query([])?,
    };
    let mut records = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let record: String = row.get(1)?;
        // Serde also accepts struct-shaped arrays; SQL ownership paths do not.
        ensure!(
            record.trim_start().starts_with('{'),
            "decode simulator record {id}: expected a JSON object"
        );
        records.push(
            serde_json::from_str(&record)
                .with_context(|| format!("decode simulator record {id}"))?,
        );
    }
    Ok(records)
}

#[cfg(test)]
mod tests;
