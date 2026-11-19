//! `[apps.<name>]`: a table of settings for an app built on Shoal, such as
//! Quay, in every config layer. Shoal reads none of it: it checks only that
//! the table is TOML, layers it as it layers its own options, a table
//! merging key by key with the one below it and any other value replacing
//! what is below, and hands the result to the app.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use toml::{Table, Value};

use crate::config::{
    options::{self, Entry, Merge, Options},
    repo::ConfigLayer as Layer,
};

/// One app's table as one layer writes it.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct AppTable(pub Table);

impl Options for AppTable {
    type Resolved = Table;

    fn built_in() -> Self {
        Self::default()
    }

    fn merge(&mut self, top: Self, _merge: Merge) {
        merge_tables(&mut self.0, top.0);
    }

    fn resolve(self, _prefix: &str) -> Result<Table> {
        Ok(self.0)
    }

    fn entries(prefix: &str, sources: &[(&Self, Layer)], entries: &mut Vec<Entry>) -> Result<()> {
        let tables: Vec<(&Table, Layer)> = (sources.iter())
            .map(|(table, layer)| (&table.0, *layer))
            .collect();
        table_entries(prefix, &tables, entries)
    }
}

/// Layer `top` over `base`: a table under a key merges with the table
/// below it, and any other value replaces what is below.
fn merge_tables(base: &mut Table, top: Table) {
    for (key, value) in top {
        match (base.get_mut(&key), value) {
            (Some(Value::Table(below)), Value::Table(above)) => merge_tables(below, above),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// Every value that is not a table, with the layer that supplied it, from
/// `sources` highest first.
fn table_entries(
    prefix: &str,
    sources: &[(&Table, Layer)],
    entries: &mut Vec<Entry>,
) -> Result<()> {
    let keys: std::collections::BTreeSet<&String> =
        sources.iter().flat_map(|(table, _)| table.keys()).collect();
    for key in keys {
        let mut values = (sources.iter())
            .filter_map(|(table, layer)| Some((table.get(key)?, *layer)))
            .peekable();
        let Some((top, layer)) = values.next() else {
            continue;
        };
        let key = options::key(prefix, key);
        let Value::Table(top) = top else {
            entries.push(Entry {
                key,
                value: serde_json::to_value(top)?,
                layer,
                file: None,
            });
            continue;
        };
        // A table merges with the tables right below it, down to the first
        // other value, which it replaces.
        let mut tables = vec![(top, layer)];
        while let Some((Value::Table(below), layer)) = values.peek() {
            tables.push((below, *layer));
            values.next();
        }
        table_entries(&key, &tables, entries)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
