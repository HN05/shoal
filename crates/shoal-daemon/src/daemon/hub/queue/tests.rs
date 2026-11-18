use rusqlite::Connection;
use serde_json::json;

use super::*;

fn database() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    for schema in [
        include_str!("../../store/hub.sql"),
        include_str!("../../store/hub_routing.sql"),
    ] {
        db.execute_batch(schema).unwrap();
    }
    db.execute_batch(
        "INSERT INTO hub_members(id, kind, name, public_key, joined_at)
         VALUES ('desk','machine','desk','k1',0), ('laptop','machine','laptop','k2',0);",
    )
    .unwrap();
    db
}

fn held(summary: &str) -> Held {
    let told = json!({
        "above": "coordinator", "message": summary,
    });
    Held {
        sender: MemberId::from("desk".to_owned()),
        sender_space: Some(SpaceId::from("child".to_owned())),
        call: Call::Tell {
            told: serde_json::from_value(told).unwrap(),
        },
        summary: summary.into(),
    }
}

fn message(call: &Call) -> String {
    match call {
        Call::Tell { told } => told.message.clone(),
        call => panic!("held {call:?}"),
    }
}

#[test]
fn a_machine_gets_what_the_hub_held_for_it_oldest_first() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let laptop = MemberId::from("laptop".to_owned());
    let (now, expires) = (UnixTime::now(), UnixTime::now() + LIFETIME);
    push(&tx, &laptop, &held("first"), now, expires).unwrap();
    push(&tx, &laptop, &held("second"), now, expires).unwrap();
    let desk = MemberId::from("desk".to_owned());
    assert!(first(&tx, &desk).unwrap().is_none());
    let (seq, call) = first(&tx, &laptop).unwrap().unwrap();
    assert_eq!(message(&call.held.call), "first");
    assert_eq!(call.held.sender, MemberId::from("desk".to_owned()));
    remove(&tx, seq).unwrap();
    let (_, call) = first(&tx, &laptop).unwrap().unwrap();
    assert_eq!(message(&call.held.call), "second");
}

#[test]
fn the_hub_drops_only_what_expired_with_whom_it_was_for() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let laptop = MemberId::from("laptop".to_owned());
    let now = UnixTime::now();
    let lapsed_at = now - Duration::from_secs(60);
    push(&tx, &laptop, &held("old"), lapsed_at - LIFETIME, lapsed_at).unwrap();
    push(&tx, &laptop, &held("new"), now, now + LIFETIME).unwrap();
    let lapsed = take_expired(&tx, now).unwrap();
    assert_eq!(lapsed.len(), 1);
    assert_eq!(lapsed[0].machine, laptop);
    assert_eq!(lapsed[0].held.summary, "old");
    assert_eq!(lapsed[0].held.sender_space, held("old").sender_space);
    let (_, call) = first(&tx, &laptop).unwrap().unwrap();
    assert_eq!(message(&call.held.call), "new");
    forget(&tx, &laptop).unwrap();
    assert!(first(&tx, &laptop).unwrap().is_none());
}
