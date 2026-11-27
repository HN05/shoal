//! Migration 320: the approval requests' columns, filled from their records.
use super::*;
use serde_json::{Value, json};

/// A database at the floor with a repository and its workspace, and the
/// requests `records`, each stored under its key with the owner it names.
fn at_floor_with(records: &[Value]) -> Result<Connection> {
    let db = at_floor()?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        BEGIN;
        INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
            VALUES ('work','/work','worker','ready','abc','/admin');
        INSERT INTO spaces(id,kind,repository_id,name,number)
            VALUES ('work','workspace','repo','worker',3);
        COMMIT;",
    )?;
    for record in records {
        let owned = record["repository_id"].is_string();
        db.execute(
            "INSERT INTO access_requests(id,space_id,repository_id,target_key,name,record,active,
                    space_number,number)
                VALUES (?1,?2,?3,?4,?5,?6,?7,3,?8)",
            rusqlite::params![
                record["key"].as_str(),
                (!owned).then_some("work"),
                record["repository_id"].as_str(),
                record["target"].as_str(),
                record["name"].as_str(),
                record.to_string(),
                record["active"].as_bool(),
                record["item"].as_i64(),
            ],
        )?;
    }
    Ok(db)
}

/// A request's record as Shoal before migration 320 stored it.
fn stored(key: &str, item: i64, extra: Value) -> Value {
    let mut record = json!({
        "space_number": 3, "item": item, "number": format!("3.{item}"), "key": key,
        "space_id": "work", "space": "worker", "target": "push", "name": "refs/heads/worker",
        "specification": {"kind": "push"}, "lifetime": "request", "reason": "ship it",
        "status": "pending", "created_at": 1, "decided_at": null, "active": true,
    });
    (record.as_object_mut().unwrap()).extend(extra.as_object().unwrap().clone());
    record
}

#[test]
fn approval_requests_keep_their_queried_fields_in_columns() -> Result<()> {
    let mut db = at_floor_with(&[
        stored(
            "escalated",
            1,
            json!({"decider": "work", "routed": true, "operation": "push-1",
                "gate": {"state": "escalated", "reason": "unsure", "rule": "ask"}}),
        ),
        stored(
            "proposal",
            2,
            json!({"repository_id": "repo", "target": "issue_open", "name": "issue",
                "status": "approved", "gate": {"state": "allowed"}, "space_removed": true}),
        ),
        stored("plain", 3, json!({"status": "withdrawn", "active": false})),
    ])?;
    migrate(&mut db)?;
    let mut reading = db.prepare(
        "SELECT id,space_id,proposer_id,repository_id,status,decider,gate,operation,record
            FROM access_requests ORDER BY rowid",
    )?;
    let rows = reading
        .query_map([], |row| {
            let columns = (0..8)
                .map(|column| row.get::<_, Option<String>>(column))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((columns, row.get::<_, String>(8)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let rows = (rows.into_iter())
        .map(|(columns, record)| Ok((columns, serde_json::from_str(&record)?)))
        .collect::<Result<Vec<(Vec<Option<String>>, Value)>>>()?;
    let record = |extra: Value| {
        let mut record = json!({
            "space": "worker", "specification": {"kind": "push"}, "lifetime": "request",
            "reason": "ship it", "created_at": 1, "decided_at": null,
        });
        (record.as_object_mut().unwrap()).extend(extra.as_object().unwrap().clone());
        record
    };
    let columns = |values: [Option<&str>; 8]| values.map(|value| value.map(str::to_owned)).to_vec();
    assert_eq!(
        rows,
        [
            (
                columns([
                    Some("escalated"),
                    Some("work"),
                    None,
                    None,
                    Some("pending"),
                    Some("work"),
                    Some("escalated"),
                    Some("push-1")
                ]),
                record(json!({"routed": true, "gate": {"reason": "unsure", "rule": "ask"}}))
            ),
            (
                columns([
                    Some("proposal"),
                    None,
                    Some("work"),
                    Some("repo"),
                    Some("approved"),
                    None,
                    Some("allowed"),
                    None
                ]),
                record(json!({}))
            ),
            (
                columns([
                    Some("plain"),
                    Some("work"),
                    None,
                    None,
                    Some("withdrawn"),
                    None,
                    None,
                    None
                ]),
                record(json!({}))
            ),
        ]
    );
    Ok(())
}

#[test]
fn a_request_whose_record_lacks_a_moved_field_fails_the_migration() -> Result<()> {
    let mut missing = stored("missing", 1, json!({}));
    missing.as_object_mut().unwrap().remove("status");
    let mut db = at_floor_with(&[missing])?;
    let error = format!("{:#}", migrate(&mut db).unwrap_err());
    assert!(
        error.contains("access_requests row missing") && error.contains("no status"),
        "{error}"
    );
    Ok(())
}

#[test]
fn a_request_with_an_unknown_gate_state_fails_the_migration() -> Result<()> {
    let mut db = at_floor_with(&[stored(
        "unknown",
        1,
        json!({"gate": {"state": "pondering"}}),
    )])?;
    let error = format!("{:#}", migrate(&mut db).unwrap_err());
    assert!(
        error.contains("access_requests row unknown") && error.contains("pondering"),
        "{error}"
    );
    Ok(())
}
