//! What the migrations that move fields of stored JSON into columns share:
//! a row's JSON object, whose fields they take out, refusing a row that is
//! malformed or lacks one.
use anyhow::{Context, Result, bail};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

/// The JSON object a row of `table` holds, named by `key` in errors.
pub(super) struct JsonRow {
    table: &'static str,
    key: String,
    object: Map<String, Value>,
}

impl JsonRow {
    pub(super) fn parse(table: &'static str, key: impl Into<String>, text: &str) -> Result<Self> {
        let key = key.into();
        let Ok(Value::Object(object)) = serde_json::from_str(text) else {
            bail!("{table} row {key} holds no JSON object");
        };
        Ok(Self { table, key, object })
    }

    /// Takes out the field at `path` as a `T`; refuses a row that lacks it
    /// or holds a value `T` does not take.
    pub(super) fn take<T: DeserializeOwned>(&mut self, path: &[&str]) -> Result<T> {
        let value = self.take_optional(path)?;
        value.with_context(|| format!("{} row {} lacks {}", self.table, self.key, path.join(".")))
    }

    /// Takes out the field at `path` as a `T`, absent where it or a parent
    /// is, or where it is null. Refuses a parent that is no object, and a
    /// value `T` does not take.
    pub(super) fn take_optional<T: DeserializeOwned>(
        &mut self,
        path: &[&str],
    ) -> Result<Option<T>> {
        let Some(value) = self.take_value(path)? else {
            return Ok(None);
        };
        let field = path.join(".");
        serde_json::from_value(value).with_context(|| {
            format!(
                "{} row {}: {field} holds no value it takes",
                self.table, self.key
            )
        })
    }

    /// Takes out the field at `path`, absent where it or a parent is, or
    /// where it is null. Refuses a parent that is no object.
    fn take_value(&mut self, path: &[&str]) -> Result<Option<Value>> {
        let (last, parents) = path.split_last().context("a field has a path")?;
        let mut object = &mut self.object;
        for parent in parents {
            match object.get_mut(*parent) {
                None | Some(Value::Null) => return Ok(None),
                Some(Value::Object(inner)) => object = inner,
                Some(_) => bail!("{} row {}: {parent} is no object", self.table, self.key),
            }
        }
        Ok(object.remove(*last).filter(|value| !value.is_null()))
    }

    /// Takes out the field at `path`, which must hold `column`, the value
    /// the row keeps in a column already; absent only where that is null.
    pub(super) fn take_matching(&mut self, path: &[&str], column: &Value) -> Result<()> {
        match self.take_value(path)? {
            None if column.is_null() => Ok(()),
            Some(value) if value == *column => Ok(()),
            value => bail!(
                "{} row {} holds {} in {} but {column} in its column",
                self.table,
                self.key,
                value.unwrap_or_default(),
                path.join("."),
            ),
        }
    }

    pub(super) fn text(&self) -> String {
        Value::Object(self.object.clone()).to_string()
    }
}

/// The name a value of a closed set has in JSON, which its column keeps.
pub(super) fn spelling<T: Serialize>(value: &T) -> Result<String> {
    match serde_json::to_value(value)? {
        Value::String(name) => Ok(name),
        other => bail!("{other} is no name"),
    }
}
