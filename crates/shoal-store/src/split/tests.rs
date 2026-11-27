use super::*;
use rusqlite::{Connection, params};
use serde::Deserialize;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Outer {
    id: String,
    inner: Option<Inner>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Inner {
    number: Option<i64>,
    name: String,
}

const COLUMNS: &[Column] = &[("id", &["id"]), ("number", &["inner", "number"])];

/// What a row with `id`, `number` and the JSON `record` decodes to.
fn decoded(id: &str, number: Option<i64>, record: &str) -> rusqlite::Result<Outer> {
    let db = Connection::open_in_memory().unwrap();
    db.query_row(
        "SELECT ?1 AS id, ?2 AS number, ?3 AS record",
        params![id, number, record],
        |row| decode_row(row, "record", COLUMNS),
    )
}

#[test]
fn a_value_round_trips_through_its_columns_and_json() -> Result<()> {
    let value = Outer {
        id: "a".into(),
        inner: Some(Inner {
            number: Some(3),
            name: "n".into(),
        }),
    };
    let (record, columns) = split(&value, COLUMNS)?;
    assert_eq!(record, r#"{"inner":{"name":"n"}}"#);
    assert_eq!(columns, [SqlValue::Text("a".into()), SqlValue::Integer(3)]);
    assert_eq!(decoded("a", Some(3), &record)?, value);
    Ok(())
}

#[test]
fn a_null_column_leaves_its_field_out() -> Result<()> {
    assert_eq!(decoded("a", None, r#"{"inner":null}"#)?.inner, None);
    Ok(())
}

#[test]
fn a_column_without_its_parent_is_refused() {
    assert!(decoded("a", Some(3), r#"{"inner":null}"#).is_err());
}

#[test]
fn a_null_json_column_holds_no_value() -> rusqlite::Result<()> {
    let db = Connection::open_in_memory().unwrap();
    let value: Option<Outer> = db.query_row(
        "SELECT 'a' AS id, NULL AS number, NULL AS record",
        [],
        |row| decode_optional_row(row, "record", COLUMNS),
    )?;
    assert_eq!(value, None);
    Ok(())
}
