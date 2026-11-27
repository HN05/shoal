//! Migrations 338 and 339: a place's host, ID and instance move into
//! columns of the executions and owned places that record it.
use super::*;
use serde_json::{Value, json};

/// A Rex block with a zmx session nested in it, as an earlier Shoal
/// recorded an execution's place.
fn nested_block() -> Value {
    json!({
        "host": "rex", "place": "block:01a2", "data": {"bin_dir": "/usr/bin"},
        "nested": {"host": "zmx", "place": "chat-b559", "instance": "16435:1795721037"},
    })
}

/// A Herdr pane Shoal created under a reused ID.
fn owned_pane() -> Value {
    json!({"host": "herdr", "group": "w1", "place": "p3", "owned": true, "instance": "2"})
}

/// Each row of `query`'s columns, as text or null.
fn texts(db: &Connection, query: &str) -> Result<Vec<Vec<Option<String>>>> {
    let mut statement = db.prepare(query)?;
    let columns = statement.column_count();
    let rows = statement.query_map([], |row| {
        (0..columns).map(|column| row.get(column)).collect()
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `text` read as JSON.
fn json_of(text: &Option<String>) -> Value {
    serde_json::from_str(text.as_deref().unwrap()).unwrap()
}

#[test]
fn a_place_keeps_its_host_id_and_instance_in_columns() -> Result<()> {
    let mut db = at_floor()?;
    db.execute(
        "INSERT INTO executions(id,space_id,state,origin) VALUES
            ('agent','work','running',?1),('group','work','running','{\"host\":\"herdr\",\"group\":\"w1\"}'),
            ('bare','work','running',NULL)",
        [nested_block().to_string()],
    )?;
    db.execute(
        "INSERT INTO owned_places(host,server,place,space_id,origin) VALUES ('herdr','','p3','work',?1)",
        [owned_pane().to_string()],
    )?;

    migrate(&mut db)?;

    let executions = texts(
        &db,
        "SELECT id,origin_host,origin_place,origin_instance,origin FROM executions ORDER BY rowid",
    )?;
    let [agent, group, bare] = &executions[..] else {
        anyhow::bail!("expected three executions, found {executions:?}");
    };
    assert_eq!(
        agent[1..4],
        [Some("rex".into()), Some("block:01a2".into()), None]
    );
    assert_eq!(
        json_of(&agent[4]),
        json!({
            "data": {"bin_dir": "/usr/bin"},
            "nested": {"host": "zmx", "place": "chat-b559", "instance": "16435:1795721037"},
        })
    );
    // A group alone names no place.
    assert_eq!(group[1..4], [Some("herdr".into()), None, None]);
    assert_eq!(json_of(&group[4]), json!({"group": "w1"}));
    assert_eq!(bare[1..], [None, None, None, None]);
    let owned = texts(
        &db,
        "SELECT origin_host,origin_place,origin_instance,origin FROM owned_places",
    )?;
    assert_eq!(
        owned[0][..3],
        [Some("herdr".into()), Some("p3".into()), Some("2".into())]
    );
    assert_eq!(json_of(&owned[0][3]), json!({"group": "w1", "owned": true}));
    Ok(())
}

#[test]
fn an_owned_place_whose_json_names_another_place_stops_the_migration() -> Result<()> {
    let mut db = at_floor()?;
    db.execute(
        "INSERT INTO owned_places(host,server,place,space_id,origin) VALUES ('herdr','','p4','work',?1)",
        [owned_pane().to_string()],
    )?;

    let error = format!("{:#}", migrate(&mut db).unwrap_err());

    assert!(error.contains("owned_places row 1"), "{error}");
    Ok(())
}

#[test]
fn an_execution_place_without_a_host_stops_the_migration_naming_its_row() -> Result<()> {
    let mut db = at_floor()?;
    db.execute(
        "INSERT INTO executions(id,space_id,state,origin) VALUES ('agent','work','running','{\"place\":\"p3\"}')",
        [],
    )?;

    let error = format!("{:#}", migrate(&mut db).unwrap_err());

    assert!(error.contains("executions row agent"), "{error}");
    assert!(error.contains("names no host"), "{error}");
    Ok(())
}
