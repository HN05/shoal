//! Options that take another option's value: `{ same_as = "<key>" }` in
//! place of a value names another key of the same table. A file's
//! references are taken out before its tables are read, and followed once
//! every layer is in, so a key takes the other's final value.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use toml::de::{DeTable, DeValue};

const SAME_AS: &str = "same_as";

/// The references one file makes.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct References(Vec<Reference>);

/// `key = { same_as = "<target>" }` in the table at `table`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Reference {
    /// The keys from the file's root to the table.
    table: Vec<String>,
    key: String,
    target: String,
}

impl References {
    /// Take every reference out of `table`, a file's root or a table below
    /// it, so the rest reads as values. A table that is anything but one
    /// string `same_as` stays, for its option to read.
    pub fn take(table: &mut DeTable<'_>) -> Self {
        let mut found = Vec::new();
        take(table, &mut Vec::new(), &mut found);
        Self(found)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn take(table: &mut DeTable<'_>, path: &mut Vec<String>, found: &mut Vec<Reference>) {
    let mut references = Vec::new();
    for (key, value) in table.iter_mut() {
        let DeValue::Table(inner) = value.get_mut() else {
            continue;
        };
        let key = key.get_ref().to_string();
        match target(inner) {
            Some(target) => references.push((key, target)),
            None => {
                path.push(key);
                take(inner, path, found);
                path.pop();
            }
        }
    }
    for (key, target) in references {
        table.remove(key.as_str());
        let table = path.clone();
        found.push(Reference { table, key, target });
    }
}

/// The key `table` names when it is `{ same_as = "<key>" }`.
fn target(table: &DeTable<'_>) -> Option<String> {
    match table.get(SAME_AS).map(|value| value.get_ref()) {
        Some(DeValue::String(target)) if table.len() == 1 => Some(target.to_string()),
        _ => None,
    }
}

/// Give each key a reference of `layers`, lowest first, names the final
/// value of its target in `merged`, the layers combined as written, unless
/// a higher layer sets the key itself. A key that takes its value from a
/// key that does in turn follows it once that one has its value. `root`
/// names the keys of the files' root in errors.
pub fn follow<T: Serialize + DeserializeOwned>(
    layers: &mut [T],
    references: fn(&T) -> &References,
    merged: impl Fn(&[T]) -> Result<Value>,
    root: &str,
) -> Result<()> {
    if layers.iter().all(|layer| references(layer).is_empty()) {
        return Ok(());
    }
    let mut pending = in_effect(layers, references)?;
    while !pending.is_empty() {
        let (layer, reference) = pending.remove(ready(&pending, root)?);
        let value = reference.target_value(&merged(layers)?, root)?;
        layers[layer] = reference.give(&layers[layer], value, root)?;
    }
    Ok(())
}

/// Each reference with its layer's index that no higher layer overrides,
/// by setting its key or naming another value for it.
fn in_effect<T: Serialize>(
    layers: &[T],
    references: fn(&T) -> &References,
) -> Result<Vec<(usize, Reference)>> {
    let written = layers
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()?;
    let mut found = Vec::new();
    for (index, layer) in layers.iter().enumerate() {
        for reference in &references(layer).0 {
            let path = reference.key_path();
            let overridden = (index + 1..layers.len()).any(|above| {
                lookup(&written[above], &path).is_some_and(has_value)
                    || references(&layers[above])
                        .0
                        .iter()
                        .any(|other| other.key_path() == path)
            });
            if !overridden {
                found.push((index, reference.clone()));
            }
        }
    }
    Ok(found)
}

/// The position in `pending` of a reference whose target takes no value
/// from a reference still pending; none means they form a cycle.
fn ready(pending: &[(usize, Reference)], root: &str) -> Result<usize> {
    let waits = |reference: &Reference| {
        let target = reference.target_path();
        pending.iter().any(|(_, other)| other.key_path() == target)
    };
    if let Some(index) = pending.iter().position(|(_, reference)| !waits(reference)) {
        return Ok(index);
    }
    let (_, reference) = &pending[0];
    bail!(
        "{} = {{ same_as = {:?} }}: {} and {} form a cycle of same_as references",
        reference.key_name(root),
        reference.target,
        reference.key_name(root),
        reference.target_name(root)
    )
}

impl Reference {
    /// The target's value in `combined`, the layers combined as written.
    fn target_value(&self, combined: &Value, root: &str) -> Result<Value> {
        let value = lookup(combined, &self.target_path()).filter(|value| has_value(value));
        let value = value.with_context(|| {
            format!(
                "{} = {{ same_as = {:?} }}: {} is not set",
                self.key_name(root),
                self.target,
                self.target_name(root)
            )
        })?;
        Ok(value.clone())
    }

    /// `layer` with the key set to `value`, which must be of its type.
    fn give<T: Serialize + DeserializeOwned>(
        &self,
        layer: &T,
        value: Value,
        root: &str,
    ) -> Result<T> {
        let mut written = serde_json::to_value(layer)?;
        set(&mut written, &self.key_path(), value);
        serde_json::from_value(written).with_context(|| {
            format!(
                "{} cannot take the value of {}",
                self.key_name(root),
                self.target_name(root)
            )
        })
    }

    fn key_path(&self) -> Vec<&str> {
        self.path_to(&self.key)
    }

    fn target_path(&self) -> Vec<&str> {
        self.path_to(&self.target)
    }

    fn path_to<'a>(&'a self, key: &'a str) -> Vec<&'a str> {
        let mut path: Vec<&str> = self.table.iter().map(String::as_str).collect();
        path.push(key);
        path
    }

    fn key_name(&self, root: &str) -> String {
        dotted(root, &self.key_path())
    }

    fn target_name(&self, root: &str) -> String {
        dotted(root, &self.target_path())
    }
}

/// `path` under `root` as a dotted TOML key, quoting a key that is not bare.
fn dotted(root: &str, path: &[&str]) -> String {
    let bare = |key: &str| {
        !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    };
    let keys = path.iter().map(|key| {
        if bare(key) {
            (*key).to_owned()
        } else {
            format!("{key:?}")
        }
    });
    std::iter::once(root.to_owned())
        .filter(|root| !root.is_empty())
        .chain(keys)
        .collect::<Vec<_>>()
        .join(".")
}

fn lookup<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |value, key| value.get(key))
}

/// Set the value at `path`, adding the tables on the way that `value`
/// lacks.
fn set(value: &mut Value, path: &[&str], new: Value) {
    let Some((last, tables)) = path.split_last() else {
        return;
    };
    let mut table = value;
    for key in tables {
        if !table.get(key).is_some_and(Value::is_object) {
            table[key] = Value::Object(Default::default());
        }
        table = &mut table[key];
    }
    table[last] = new;
}

/// Whether a layer sets an option to `value`: an empty list or table
/// leaves it to the layers below, as an omitted one does.
fn has_value(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(items) => !items.is_empty(),
        Value::Object(entries) => !entries.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests;
