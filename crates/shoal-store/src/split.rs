//! Typed values whose row keeps some of their fields in columns and the
//! rest as JSON, so that each value is stored once.
use anyhow::{Context, Result, bail};
use rusqlite::{Row, types::ValueRef};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

/// A column of a row and the field of its value's JSON that the column
/// keeps, by the field's path.
pub type Column = (&'static str, &'static [&'static str]);

/// The JSON text of `value` without the fields its row keeps in `columns`.
pub fn json_without<T: Serialize>(value: &T, columns: &[Column]) -> Result<String> {
    let mut json = serde_json::to_value(value)?;
    for (_, field) in columns {
        take(&mut json, field);
    }
    Ok(serde_json::to_string(&json)?)
}

/// The value `row` keeps in the JSON column `json` and in `columns`. A null
/// column leaves its field out, as an absent optional field is. Refuses a
/// column whose field's parent the JSON lacks.
pub fn decode_row<T: DeserializeOwned>(
    row: &Row<'_>,
    json: &str,
    columns: &[Column],
) -> rusqlite::Result<T> {
    let index = row.as_ref().column_index(json)?;
    let failed = |error: anyhow::Error| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, error.into())
    };
    let mut value: Value = serde_json::from_str(&row.get::<_, String>(index)?)
        .map_err(|error| failed(error.into()))?;
    for (column, field) in columns {
        let column_value = sql_value(row.get_ref(*column)?).map_err(failed)?;
        if !column_value.is_null() {
            put(&mut value, field, column_value)
                .with_context(|| format!("put column {column} back"))
                .map_err(failed)?;
        }
    }
    serde_json::from_value(value).map_err(|error| failed(error.into()))
}

fn sql_value(value: ValueRef<'_>) -> Result<Value> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(number) => number.into(),
        ValueRef::Real(number) => number.into(),
        ValueRef::Text(text) => std::str::from_utf8(text)?.into(),
        ValueRef::Blob(_) => bail!("a column a JSON field moved into holds no blob"),
    })
}

fn take(value: &mut Value, field: &[&str]) -> Option<Value> {
    let (last, parents) = field.split_last()?;
    let mut object = value;
    for parent in parents {
        object = object.get_mut(*parent)?;
    }
    object.as_object_mut()?.remove(*last)
}

fn put(value: &mut Value, field: &[&str], column: Value) -> Result<()> {
    let Some((last, parents)) = field.split_last() else {
        bail!("a column's field has a path");
    };
    let mut object = value;
    for parent in parents {
        object = object
            .get_mut(*parent)
            .context("the JSON lacks its parent")?;
    }
    let object: &mut Map<String, Value> =
        object.as_object_mut().context("its parent is no object")?;
    object.insert((*last).to_owned(), column);
    Ok(())
}

#[cfg(test)]
mod tests;
