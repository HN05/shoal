use super::*;
use crate::daemon::store::Store;
use rusqlite::params;
use serde_json::{Value, json};

fn record(id: &str, current: Option<&str>, last: Option<&str>) -> Value {
    json!({
        "id": id, "udid": null, "device": "phone", "runtime": "ios",
        "workspace_id": current, "last_workspace_id": last,
        "lease_name": "default", "reason": null, "state": "booting",
        "last_used": 42, "error": null
    })
}

fn save(db: &Connection, record: &Value) -> Result<()> {
    db.execute(
        "INSERT INTO simulators(id,record) VALUES (?1,?2)
         ON CONFLICT(id) DO UPDATE SET record=excluded.record",
        params![record["id"].as_str().unwrap(), record.to_string()],
    )?;
    Ok(())
}

fn ids(db: &Connection, owner: Option<&str>) -> Result<Vec<String>> {
    Ok(list(db, owner)?.into_iter().map(|sim| sim.id).collect())
}

#[tokio::test]
async fn migration_and_transitions_preserve_json_ownership_and_audits() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("state.db");
    let db = Connection::open(&path)?;
    db.execute_batch(include_str!("../../../tests/fixtures/schema_v17.sql"))?;
    let mut sim = record("b", Some("A"), None);
    save(&db, &sim)?;
    save(&db, &record("a", None, Some("A")))?;
    save(&db, &record("c", None, None))?;
    db.execute("INSERT INTO simulator_clean_requests(request_id,workspace_id,record) VALUES ('audit','A','{}')", [])?;
    let before: String = db.query_row("SELECT record FROM simulators WHERE id='b'", [], |r| {
        r.get(0)
    })?;
    let store = Store::open(path.clone()).await?;
    let after: String = db.query_row("SELECT record FROM simulators WHERE id='b'", [], |r| {
        r.get(0)
    })?;
    assert_eq!(before, after);
    assert_eq!(ids(&db, Some("A"))?, ["a", "b"]);
    assert_eq!(ids(&db, None)?, ["a", "b", "c"]);
    for state in ["creating", "booting", "leased", "failed"] {
        sim["state"] = json!(state);
        save(&db, &sim)?;
        assert_eq!(ids(&db, Some("A"))?, ["a", "b"]);
    }
    sim["workspace_id"] = Value::Null;
    sim["last_workspace_id"] = json!("A");
    sim["state"] = json!("idle");
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some("A"))?, ["a", "b"]);

    // The persisted claim changes owner before boot updates the previous owner.
    sim["workspace_id"] = json!("B");
    sim["state"] = json!("booting");
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some("A"))?, ["a"]);
    assert_eq!(ids(&db, Some("B"))?, ["b"]);
    drop(store);
    Store::open(path)
        .await?
        .quarantine_interrupted_operations()
        .await?;
    assert_eq!(ids(&db, Some("B"))?, ["b"]);
    sim["last_workspace_id"] = json!("B");
    sim["state"] = json!("failed");
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some("A"))?, ["a"]);
    assert_eq!(ids(&db, Some("B"))?, ["b"]);
    sim["workspace_id"] = Value::Null;
    sim["state"] = json!("idle");
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some("B"))?, ["b"]);
    db.execute("DELETE FROM simulators WHERE id='b'", [])?;
    assert!(ids(&db, Some("B"))?.is_empty());
    assert_eq!(
        db.query_row("SELECT record FROM simulator_clean_requests", [], |r| {
            r.get::<_, String>(0)
        })?,
        "{}"
    );

    // Absent fields and empty strings retain Option<String> semantics.
    let mut absent = record("d", None, Some("A"));
    absent.as_object_mut().unwrap().remove("workspace_id");
    save(&db, &absent)?;
    save(&db, &record("e", Some(""), Some("A")))?;
    assert_eq!(ids(&db, Some("A"))?, ["a", "d"]);
    assert_eq!(ids(&db, Some(""))?, ["e"]);
    Ok(())
}

#[tokio::test]
async fn malformed_migration_rolls_back_and_names_the_record() -> Result<()> {
    for invalid in [
        "{",
        r#"["broken",null,"phone","ios","A",null,null,null,"booting",42,null,null]"#,
        r#"{"workspace_id":7}"#,
        r#"{"workspace_id":null,"workspace_id":"A"}"#,
    ] {
        let root = tempfile::tempdir()?;
        let path = root.path().join("state.db");
        let db = Connection::open(&path)?;
        db.execute_batch(include_str!("../../../tests/fixtures/schema_v17.sql"))?;
        db.execute("INSERT INTO simulators VALUES ('broken',?1)", [invalid])?;
        let error = Store::open(path.clone()).await.err().unwrap();
        assert!(format!("{error:#}").contains("decode simulator record broken"));
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            17
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='simulators_effective_owner'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(
            db.query_row("SELECT record FROM simulators", [], |r| r
                .get::<_, String>(0))?,
            invalid
        );
        db.execute("DELETE FROM simulators", [])?;
        Store::open(path).await?;
    }
    Ok(())
}

#[tokio::test]
async fn invalid_ownership_writes_fail_atomically_and_selected_records_report_errors() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let path = root.path().join("state.db");
    Store::open(path.clone()).await?;
    let db = Connection::open(path)?;
    save(&db, &record("owned", Some("A"), None))?;
    for invalid in [
        "{",
        "[]",
        "null",
        r#"{"workspace_id":7}"#,
        r#"{"workspace_id":false}"#,
        r#"{"last_workspace_id":[]}"#,
        r#"{"workspace_id":"B","last_workspace_id":{}}"#,
        r#"{"workspace_id":null,"workspace_id":"B"}"#,
        r#"{"last_workspace_id":"A","last_workspace_id":"B"}"#,
    ] {
        for sql in [
            "INSERT INTO simulators VALUES ('invalid',?1)",
            "UPDATE simulators SET record=?1 WHERE id='owned'",
        ] {
            let error = db.execute(sql, [invalid]).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("invalid simulator ownership record")
            );
            assert_eq!(ids(&db, Some("A"))?, ["owned"]);
            assert_eq!(ids(&db, None)?, ["owned"]);
        }
    }
    let mut invalid = record("broken", Some("B"), None);
    invalid["state"] = json!("unknown-state");
    save(&db, &invalid)?;
    assert_eq!(ids(&db, Some("A"))?, ["owned"]);
    for owner in [None, Some("B")] {
        let error = list(&db, owner).unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("decode simulator record broken"));
        assert!(diagnostic.contains("unknown-state"));
    }
    Ok(())
}

mod performance;
