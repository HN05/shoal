//! The SQL helpers every record function shares.
use anyhow::Result;
use rusqlite::{Connection, Params, Row};
use serde::{Serialize, de::DeserializeOwned};
use shoal_core::config::permissions::PermissionsConfig;

pub fn exists(db: &Connection, sql: &str, params: impl Params) -> Result<bool> {
    Ok(db
        .prepare_cached(&format!("SELECT EXISTS({sql})"))?
        .query_row(params, |row| row.get(0))?)
}

pub fn json_text<T: Serialize>(value: Option<&T>) -> Result<Option<String>> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(Into::into)
}

/// The column value of a `--permission` narrowing: its JSON, or none for
/// one that narrows nothing.
pub fn narrowing_column(narrowing: &PermissionsConfig) -> Result<Option<String>> {
    Ok((!narrowing.is_empty())
        .then(|| serde_json::to_string(narrowing))
        .transpose()?)
}

/// Parse an optional JSON text column, reporting failures as SQL conversions.
pub fn json_column<T: DeserializeOwned>(row: &Row<'_>, name: &str) -> rusqlite::Result<Option<T>> {
    let index = row.as_ref().column_index(name)?;
    row.get::<_, Option<String>>(index)?
        .map(|json| {
            serde_json::from_str(&json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}
