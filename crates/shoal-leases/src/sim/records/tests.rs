use super::*;
use crate::store::Store;
use rusqlite::params;
use serde_json::{Value, json};
use shoal_ids::space_ids::SpaceId;
use shoal_store::test_support::open_database;

fn record(id: &str, current: Option<&str>, last: Option<&str>) -> Value {
    json!({
        "id": id, "udid": null, "device": "phone", "runtime": "ios",
        "workspace_id": current, "last_workspace_id": last,
        "lease_name": "default", "reason": null, "state": "booting",
        "last_used": 42, "error": null, "acquirer": {"kind": "user"}
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

fn ids(db: &Connection, owner: Option<&SpaceId>) -> Result<Vec<String>> {
    Ok(list(db, owner)?.into_iter().map(|sim| sim.id).collect())
}

#[tokio::test]
async fn transitions_and_reopens_preserve_json_ownership_and_audits() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("state.db");
    let store = Store::open(path.clone()).await?;
    let db = open_database(&path)?;
    let mut sim = record("b", Some("A"), None);
    save(&db, &sim)?;
    save(&db, &record("a", None, Some("A")))?;
    save(&db, &record("c", None, None))?;
    db.execute("INSERT INTO simulator_clean_requests(request_id,workspace_id,record) VALUES ('audit','A','{}')", [])?;
    assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["a", "b"]);
    assert_eq!(ids(&db, None)?, ["a", "b", "c"]);
    for state in ["creating", "booting", "leased", "failed"] {
        sim["state"] = json!(state);
        save(&db, &sim)?;
        assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["a", "b"]);
    }
    sim["workspace_id"] = Value::Null;
    sim["last_workspace_id"] = json!("A");
    sim["state"] = json!("idle");
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["a", "b"]);

    // The persisted claim changes owner before boot updates the previous owner.
    sim["workspace_id"] = json!("B");
    sim["state"] = json!("booting");
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["a"]);
    assert_eq!(ids(&db, Some(&SpaceId::from("B")))?, ["b"]);
    drop(store);
    drop(Store::open(path).await?);
    assert_eq!(ids(&db, Some(&SpaceId::from("B")))?, ["b"]);
    sim["last_workspace_id"] = json!("B");
    sim["state"] = json!("failed");
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["a"]);
    assert_eq!(ids(&db, Some(&SpaceId::from("B")))?, ["b"]);
    sim["workspace_id"] = Value::Null;
    sim["state"] = json!("idle");
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some(&SpaceId::from("B")))?, ["b"]);
    db.execute("DELETE FROM simulators WHERE id='b'", [])?;
    assert!(ids(&db, Some(&SpaceId::from("B")))?.is_empty());
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
    assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["a", "d"]);
    assert_eq!(ids(&db, Some(&SpaceId::from("")))?, ["e"]);
    Ok(())
}

#[tokio::test]
async fn invalid_ownership_writes_fail_atomically_and_selected_records_report_errors() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let path = root.path().join("state.db");
    Store::open(path.clone()).await?;
    let db = open_database(path)?;
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
            assert!(db.execute(sql, [invalid]).is_err(), "{sql} {invalid}");
            assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["owned"]);
            assert_eq!(ids(&db, None)?, ["owned"]);
        }
    }
    let mut invalid = record("broken", Some("B"), None);
    invalid["state"] = json!("unknown-state");
    save(&db, &invalid)?;
    assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["owned"]);
    for owner in [None, Some(SpaceId::from("B"))] {
        let error = list(&db, owner.as_ref()).unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("decode simulator record broken"));
        assert!(diagnostic.contains("unknown-state"));
    }
    Ok(())
}

mod performance;
