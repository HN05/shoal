//! How the store keeps a shared item: its key as text and its value as
//! JSON, none for a removed item.
use anyhow::Result;
use rusqlite::Row;

use shoal_protocol::hub::sharing;

use super::{Copy, Kind};

/// The key, value and revision columns of a row.
pub(super) type Columns = (String, Option<String>, i64);

/// The key, value and revision columns of `row`.
pub(super) fn row(row: &Row<'_>) -> rusqlite::Result<Columns> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}

/// The copy of `S` the `columns` of a row hold.
pub(super) fn copy<S: Kind>((key, value, revision): Columns) -> Result<Copy<S>> {
    Ok(sharing::Copy {
        key: key.parse()?,
        value: decode::<S>(value)?,
        revision,
    })
}

/// The JSON that keeps `value`.
pub(super) fn encode<S: Kind>(value: Option<&S::Value>) -> Result<Option<String>> {
    Ok(value.map(serde_json::to_string).transpose()?)
}

/// The value the JSON `value` keeps.
pub(super) fn decode<S: Kind>(value: Option<String>) -> Result<Option<S::Value>> {
    Ok(value
        .map(|value| serde_json::from_str(&value))
        .transpose()?)
}
