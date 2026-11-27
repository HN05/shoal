//! What the migrations that move fields of stored JSON into columns share:
//! a row's JSON object, whose fields they take out, refusing a row that is
//! malformed or lacks one.
use anyhow::{Context, Result, bail};
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

    /// Takes out the field at `path`, absent where it or a parent is, or
    /// where it is null. Refuses a parent that is no object.
    pub(super) fn take_optional(&mut self, path: &[&str]) -> Result<Option<Value>> {
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
        match self.take_optional(path)? {
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
