//! Typed values whose row keeps some of their fields in columns and the
//! rest as JSON, so that each value is stored once.
use anyhow::{Context, Result, bail};
use rusqlite::{
    Row,
    types::{Value as SqlValue, ValueRef},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

/// A column of a row and the field of its value's JSON that the column
/// keeps, by the field's path.
pub type Column = (&'static str, &'static [&'static str]);

/// The JSON text of `value` without the fields its row keeps in `columns`.
pub fn json_without<T: Serialize>(value: &T, columns: &[Column]) -> Result<String> {
    Ok(split(value, columns)?.0)
}

/// The JSON text of `value` without the fields its row keeps in `columns`,
/// and the value of each column, in their order: null for an absent field.
pub fn split<T: Serialize>(value: &T, columns: &[Column]) -> Result<(String, Vec<SqlValue>)> {
    let mut json = serde_json::to_value(value)?;
    let values = (columns.iter())
        .map(|(column, field)| {
            column_value(take(&mut json, field).unwrap_or_default())
                .with_context(|| format!("fill column {column}"))
        })
        .collect::<Result<_>>()?;
    Ok((serde_json::to_string(&json)?, values))
}

/// What [`decode_row`] reads, or none where the JSON column is null.
pub fn decode_optional_row<T: DeserializeOwned>(
    row: &Row<'_>,
    json: &str,
    columns: &[Column],
) -> rusqlite::Result<Option<T>> {
    match row.get_ref(json)? {
        ValueRef::Null => Ok(None),
        _ => decode_row(row, json, columns).map(Some),
    }
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

fn column_value(value: Value) -> Result<SqlValue> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Bool(flag) => SqlValue::Integer(flag.into()),
        Value::Number(number) => match number.as_i64() {
            Some(integer) => SqlValue::Integer(integer),
            None => SqlValue::Real(number.as_f64().context("a number SQLite holds")?),
        },
        Value::String(text) => SqlValue::Text(text),
        Value::Array(_) | Value::Object(_) => bail!("a column holds no array or object"),
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
