use rusqlite::Connection;
use serde_json::json;

use super::*;

fn database() -> Connection {
    let db = shoal_store::test_support::migrated().unwrap();
    db.execute_batch(
        "INSERT INTO hub_members(id, kind, name, public_key, joined_at)
         VALUES ('desk','machine','desk','k1',0), ('laptop','machine','laptop','k2',0);",
    )
    .unwrap();
    db
}

fn registered(id: &str, name: &str) -> RegisteredRepository {
    serde_json::from_value(json!({"id": id, "name": name})).unwrap()
}

fn replaced(db: &mut Connection, member: &str, repositories: &[RegisteredRepository]) {
    let tx = db.transaction().unwrap();
    replace(&tx, &MemberId::from(member.to_owned()), repositories).unwrap();
    tx.commit().unwrap();
}

/// Each listed repository's machine and name.
fn listed(db: &mut Connection) -> Vec<(String, String)> {
    let tx = db.transaction().unwrap();
    (read(&tx).unwrap().into_iter())
        .map(|listed| {
            (
                listed.machine.to_string(),
                listed.repository.repository.name,
            )
        })
        .collect()
}

#[test]
fn a_machine_s_report_replaces_its_repositories_alone() {
    let mut db = database();
    replaced(
        &mut db,
        "desk",
        &[registered("1", "web"), registered("2", "app")],
    );
    replaced(&mut db, "laptop", &[registered("1", "web")]);
    replaced(&mut db, "desk", &[registered("3", "api")]);
    assert_eq!(
        listed(&mut db),
        [
            ("desk".into(), "api".into()),
            ("laptop".into(), "web".into())
        ]
    );
}

#[test]
fn forgetting_a_machine_drops_its_repositories() {
    let mut db = database();
    replaced(&mut db, "desk", &[registered("1", "web")]);
    replaced(&mut db, "laptop", &[registered("1", "web")]);
    let tx = db.transaction().unwrap();
    crate::directory::forget(&tx, &MemberId::from("desk".to_owned())).unwrap();
    tx.commit().unwrap();
    assert_eq!(listed(&mut db), [("laptop".into(), "web".into())]);
}
