//! Small, format-preserving edits shared by global and saved repository config.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Key, Table, Value};

/// One key to set, or to remove when `value` is `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub key: String,
    pub value: Option<String>,
}

/// Apply `changes` in order, so settings that are only valid together are
/// validated once, after the last one.
pub fn apply(text: &str, changes: &[Change]) -> Result<String> {
    changes.iter().try_fold(text.to_owned(), |text, change| {
        edit(&text, &change.key, change.value.as_deref())
            .with_context(|| format!("edit {}", change.key))
    })
}

pub fn edit(text: &str, key: &str, value: Option<&str>) -> Result<String> {
    let keys = Key::parse(key).context("expected a TOML dotted key")?;
    let (last, parents) = keys.split_last().context("config key cannot be empty")?;
    let mut document: DocumentMut = text.parse().context("parse existing config")?;
    let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
    for key in parents {
        if !table.contains_key(key.get()) && value.is_some() {
            let mut child = Table::new();
            child.set_implicit(true);
            table.insert(key.get(), Item::Table(child));
        }
        table = table
            .get_mut(key.get())
            .and_then(Item::as_table_like_mut)
            .with_context(|| format!("config key {} is missing or is not a table", key.get()))?;
    }
    if let Some(raw) = value {
        // Shell-friendly strings; arrays, booleans and numbers retain TOML types.
        let mut value = raw.parse::<Value>().unwrap_or_else(|_| Value::from(raw));
        if let Some(previous) = table.get(last.get()).and_then(Item::as_value) {
            *value.decor_mut() = previous.decor().clone();
        }
        if let Some(existing) = table.get_mut(last.get()) {
            *existing = Item::Value(value);
        } else {
            table.insert(last.get(), Item::Value(value));
        }
    } else {
        ensure!(
            table.remove(last.get()).is_some(),
            "config key {key} is not set in this layer"
        );
    }
    Ok(document.to_string())
}

#[cfg(test)]
mod tests;
