//! `[apps.<name>]`: a table of settings for an app built on Shoal, such as
//! Quay, in every config layer. Shoal reads none of it: it checks only that
//! the table is TOML, layers it as it layers its own options, a table
//! merging key by key with the one below it and any other value replacing
//! what is below, and hands the result to the app.
use std::collections::BTreeSet;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use toml::{Table, Value};

use crate::config::{
    options::{self, Entry, Merge, Options},
    repo::ConfigLayer as Layer,
};

/// One app's table as one layer writes it, or as several merged.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct AppTable {
    table: Table,
    /// The key paths where a merge put a table over another value, which
    /// hides what every layer below has there too, since layers merge in
    /// groups, such as the repository's before they go over the global
    /// config.
    #[serde(skip)]
    replaced: BTreeSet<Vec<String>>,
}

impl Options for AppTable {
    type Resolved = Table;

    fn built_in() -> Self {
        Self::default()
    }

    fn merge(&mut self, top: Self, _merge: Merge) {
        let Self { table, replaced } = top;
        for path in &replaced {
            remove(&mut self.table, path);
        }
        merge_tables(&mut self.table, table, &mut Vec::new(), &mut self.replaced);
        self.replaced.extend(replaced);
    }

    fn resolve(self, _prefix: &str) -> Result<Table> {
        Ok(self.table)
    }

    fn entries(prefix: &str, sources: &[(&Self, Layer)], entries: &mut Vec<Entry>) -> Result<()> {
        let tables: Vec<(&Table, Layer)> = (sources.iter())
            .map(|(table, layer)| (&table.table, *layer))
            .collect();
        table_entries(prefix, &tables, entries)
    }
}

/// Layer `top` over `base`, both under the key path `path`: a table under
/// a key merges with the table below it, and any other value replaces what
/// is below. Each path where a table replaced another value goes into
/// `replaced`.
fn merge_tables(
    base: &mut Table,
    top: Table,
    path: &mut Vec<String>,
    replaced: &mut BTreeSet<Vec<String>>,
) {
    for (key, value) in top {
        path.push(key.clone());
        match (base.get_mut(&key), value) {
            (Some(Value::Table(below)), Value::Table(above)) => {
                merge_tables(below, above, path, replaced)
            }
            (below, value) => {
                if below.is_some() && value.is_table() {
                    replaced.insert(path.clone());
                }
                base.insert(key, value);
            }
        }
        path.pop();
    }
}

/// Remove the value under the key path `path` from `table`.
fn remove(table: &mut Table, path: &[String]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut table = table;
    for key in parents {
        match table.get_mut(key) {
            Some(Value::Table(below)) => table = below,
            _ => return,
        }
    }
    table.remove(last);
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
