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

fn alert(id: i64, message: &str) -> Alert {
    serde_json::from_value(json!({
        "id": id, "created_at": 0, "kind": "message", "sender": "user",
        "message": message, "read": false,
    }))
    .unwrap()
}

/// What `member`'s report of `users` changed of the hub's copies.
fn report(db: &mut Connection, member: &str, users: UsersAlerts) -> Option<AlertCopies> {
    let tx = db.transaction().unwrap();
    let changed = record(&tx, &MemberId::from(member.to_owned()), users).unwrap();
    tx.commit().unwrap();
    changed
}

#[test]
fn a_report_changes_only_the_copies_that_differ_and_names_their_machine() {
    let mut db = database();
    let whole = UsersAlerts::Whole(vec![alert(1, "one"), alert(2, "two")]);
    report(&mut db, "desk", whole);
    // A whole report replaces what the hub had: the alert it leaves out
    // goes, and one as it was changes nothing.
    let again = UsersAlerts::Whole(vec![alert(2, "two"), alert(3, "three")]);
    let Some(AlertCopies::Changed {
        changed, removed, ..
    }) = report(&mut db, "desk", again)
    else {
        panic!("no change");
    };
    let ids: Vec<i64> = changed.iter().map(|alert| alert.id).collect();
    assert_eq!((ids, removed), (vec![3], vec![1]));
    assert_eq!(changed[0].machine.as_ref().unwrap().name, "desk");
    let unchanged = UsersAlerts::Changes {
        changed: vec![alert(3, "three")],
        removed: Vec::new(),
    };
    assert_eq!(report(&mut db, "desk", unchanged), None);
}

#[test]
fn a_machine_reads_every_other_machine_s_copies_until_the_hub_forgets_them() {
    let mut db = database();
    report(&mut db, "desk", UsersAlerts::Whole(vec![alert(1, "desk")]));
    report(
        &mut db,
        "laptop",
        UsersAlerts::Whole(vec![alert(2, "laptop")]),
    );
    let laptop = MemberId::from("laptop".to_owned());
    let read = |db: &Connection| -> Vec<i64> {
        let others = others(db, &laptop).unwrap();
        others.iter().map(|alert| alert.id).collect()
    };
    assert_eq!(read(&db), [1]);
    let tx = db.transaction().unwrap();
    forget(&tx, &MemberId::from("desk".to_owned())).unwrap();
    tx.commit().unwrap();
    assert!(read(&db).is_empty());
}
