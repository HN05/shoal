use rusqlite::Connection;
use serde_json::json;
use shoal_protocol::hub::directory::Report;

use super::*;

fn database() -> Connection {
    let mut db = shoal_store::test_support::migrated().unwrap();
    db.execute_batch(
        "INSERT INTO hub_members(id, kind, name, public_key, joined_at)
         VALUES ('desk','machine','desk','k1',0), ('laptop','machine','laptop','k2',0);",
    )
    .unwrap();
    report(&mut db, "desk", "a");
    db
}

/// `member`'s report of its one space `id`.
fn report(db: &mut Connection, member: &str, id: &str) {
    let space = serde_json::from_value(json!({
        "id": id, "number": 1, "name": "moving", "kind": "chat", "status": "working",
    }))
    .unwrap();
    let tx = db.transaction().unwrap();
    let member = MemberId::from(member.to_owned());
    records::apply(&tx, &member, Report::Whole(vec![space])).unwrap();
    tx.commit().unwrap();
}

fn ids() -> (SpaceId, MemberId) {
    ("a".to_owned().into(), "laptop".to_owned().into())
}

#[test]
fn a_move_settled_unclaimed_refuses_the_later_claims_of_its_ticket_alone() {
    let mut db = database();
    let (space, laptop) = ids();
    let (settled, next) = (MoveTicket::generate(), MoveTicket::generate());
    let tx = db.transaction().unwrap();
    assert!(!settle(&tx, &space, &laptop, &settled).unwrap());
    assert!(ensure_unsettled(&tx, &settled).is_err());
    assert!(ensure_unsettled(&tx, &next).is_ok());
}

#[test]
fn a_move_settles_as_arrived_once_its_destination_claimed_the_space() {
    let mut db = database();
    let (space, laptop) = ids();
    let ticket = MoveTicket::generate();
    let tx = db.transaction().unwrap();
    records::claim(&tx, &laptop, &space).unwrap();
    assert!(settle(&tx, &space, &laptop, &ticket).unwrap());
    assert!(ensure_unsettled(&tx, &ticket).is_ok());
    tx.commit().unwrap();
    // Its report spends the claim, and it owns the space from then on.
    report(&mut db, "laptop", "a");
    let tx = db.transaction().unwrap();
    assert!(settle(&tx, &space, &laptop, &MoveTicket::generate()).unwrap());
}
