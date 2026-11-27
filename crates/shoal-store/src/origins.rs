//! How a row keeps a place in a terminal host, as executions and owned
//! places do: its host, ID and instance in the columns `origin_host`,
//! `origin_place` and `origin_instance`, which queries match, and the rest in
//! `origin` as the JSON of a [`StoredOrigin`].
use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::Row;
use serde::{Deserialize, Serialize};
use shoal_protocol::sessions::{HostName, Origin};

use crate::sql::json_column;

/// The columns that keep a place.
pub struct OriginColumns {
    pub host: HostName,
    pub place: Option<String>,
    pub instance: Option<String>,
    pub origin: String,
}

impl OriginColumns {
    /// The columns that keep `origin`.
    pub fn of(origin: &Origin) -> Result<Self> {
        let Origin {
            host,
            group,
            place,
            data,
            owned,
            instance,
            nested,
        } = origin;
        let stored = StoredOrigin {
            group: group.clone(),
            data: data.clone(),
            owned: *owned,
            nested: nested.clone(),
        };
        Ok(Self {
            host: *host,
            place: place.clone(),
            instance: instance.clone(),
            origin: serde_json::to_string(&stored)?,
        })
    }
}

/// The place `row` keeps, if it keeps one.
pub fn origin(row: &Row<'_>) -> rusqlite::Result<Option<Origin>> {
    let Some(stored) = json_column::<StoredOrigin>(row, "origin")? else {
        return Ok(None);
    };
    let StoredOrigin {
        group,
        data,
        owned,
        nested,
    } = stored;
    Ok(Some(Origin {
        host: row.get("origin_host")?,
        group,
        place: row.get("origin_place")?,
        data,
        owned,
        instance: row.get("origin_instance")?,
        nested,
    }))
}

/// What a row keeps as JSON of an [`Origin`].
#[derive(Serialize, Deserialize)]
struct StoredOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    data: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    owned: bool,
    /// The session of another host nested in the place, which the row
    /// keeps whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nested: Option<Box<Origin>>,
}

#[cfg(test)]
mod tests;
