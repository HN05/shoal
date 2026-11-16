//! Small, format-preserving edits shared by global and local config.
use anyhow::{Context, Result, bail, ensure};
use toml_edit::{DocumentMut, Item, Key, Table, Value};

use crate::config::permissions::{Acting, PermissionKey, Permissions};
pub use shoal_protocol::config::edit::Change;

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

/// Refuse `changes` to a config file, made by a scoped process whose space
/// holds `held`, where one sets a permission wider than it holds, a mode's
/// or a trigger's, or removes one or a table of them, which would give the
/// value below it that the process cannot check it holds, or changes a
/// trigger without holding `triggers` at `direct`; it changes other keys
/// freely. Keys and values read as [`edit`] reads them.
pub fn ensure_permissions_held(changes: &[Change], held: &Permissions) -> Result<()> {
    for change in changes {
        let keys = Key::parse(&change.key).context("expected a TOML dotted key")?;
        let path: Vec<&str> = keys.iter().map(Key::get).collect();
        if path.first() == Some(&"triggers") {
            ensure_changes_triggers(&change.key, held)?;
        }
        let key = match path.as_slice() {
            ["permissions", key]
            | ["modes", _, "permissions", key]
            | ["triggers", _, "permission", key] => *key,
            ["permissions"]
            | ["modes" | "triggers"]
            | ["modes" | "triggers", _]
            | ["modes", _, "permissions"]
            | ["triggers", _, "permission"] => bail!(
                "{}: a scoped process sets permissions one key at a time",
                change.key
            ),
            _ => continue,
        };
        let Some(key) = PermissionKey::ALL
            .iter()
            .find(|known| known.as_str() == key)
        else {
            continue;
        };
        let Some(raw) = &change.value else {
            bail!(
                "{}: removing a permission gives the value below it, which a scoped process cannot check it holds; set a value it holds instead",
                change.key
            );
        };
        let parsed = raw.parse::<Value>().ok();
        let value = parsed.as_ref().and_then(Value::as_str).unwrap_or(raw);
        let wanted = (Acting::ALL.iter())
            .find(|acting| acting.as_str() == value)
            .with_context(|| format!("{}: unknown permission value {value}", change.key))?;
        ensure!(
            *wanted <= held.acting(*key),
            "{} = \"{value}\" gives more than this space holds: {}; a space sets a permission only to a value it holds",
            change.key,
            held.cited(*key)
        );
    }
    Ok(())
}

/// Refuse a change to the trigger `key` names by a scoped process whose
/// space holds `held`, unless that holds `triggers` at `direct`: a trigger
/// starts top-level spaces as the user, in whatever mode it names, beyond
/// what the process's own space holds.
fn ensure_changes_triggers(key: &str, held: &Permissions) -> Result<()> {
    ensure!(
        held.acting(PermissionKey::Triggers) == Acting::Direct,
        "{key}: a trigger starts top-level spaces as the user, so a scoped process changes one only where its triggers permission is \"direct\": {}",
        held.cited(PermissionKey::Triggers)
    );
    Ok(())
}

#[cfg(test)]
mod tests;
