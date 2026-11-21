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

fn held(summary: &str) -> Held {
    let told = json!({
        "above": "coordinator", "for_user": false,
        "record": {"alert": {"kind": "approval_decided", "message": summary}},
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
        Call::Tell { told } => told.record.texts().1,
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
    assert!(claim_first(&tx, &desk, now).unwrap().is_none());
    let (seq, call) = claim_first(&tx, &laptop, now).unwrap().unwrap();
    assert_eq!(message(&call.held.call), "first");
    assert_eq!(call.held.sender, desk);
    // Claimed, it is not handed out again.
    let (_, call) = claim_first(&tx, &laptop, now).unwrap().unwrap();
    assert_eq!(message(&call.held.call), "second");
    remove(&tx, seq).unwrap();
}

#[test]
fn a_claim_that_never_left_goes_again_and_one_left_by_a_stop_never_does() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let laptop = MemberId::from("laptop".to_owned());
    let (now, expires) = (UnixTime::now(), UnixTime::now() + LIFETIME);
    push(&tx, &laptop, &held("unsent"), now, expires).unwrap();
    push(&tx, &laptop, &held("uncertain"), now, expires).unwrap();
    let (unsent, _) = claim_first(&tx, &laptop, now).unwrap().unwrap();
    release(&tx, unsent).unwrap();
    let (_, again) = claim_first(&tx, &laptop, now).unwrap().unwrap();
    assert_eq!(message(&again.held.call), "unsent");
    claim_first(&tx, &laptop, now).unwrap().unwrap();
    let left: Vec<String> = (take_claimed(&tx, &laptop).unwrap().iter())
        .map(|lapsed| message(&lapsed.held.call))
        .collect();
    assert_eq!(left, ["unsent", "uncertain"]);
    assert!(claim_first(&tx, &laptop, now).unwrap().is_none());
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
    // Expired, it is never handed out, though not yet dropped.
    let (_, call) = claim_first(&tx, &laptop, now).unwrap().unwrap();
    assert_eq!(message(&call.held.call), "new");
    release(&tx, 2).unwrap();
    let lapsed = take_expired(&tx, now).unwrap();
    assert_eq!(lapsed.len(), 1);
    assert_eq!(lapsed[0].machine, laptop);
    assert_eq!(lapsed[0].held.summary, "old");
    assert_eq!(lapsed[0].held.sender_space, held("old").sender_space);
    let (_, call) = claim_first(&tx, &laptop, now).unwrap().unwrap();
    assert_eq!(message(&call.held.call), "new");
    forget(&tx, &laptop).unwrap();
    assert!(claim_first(&tx, &laptop, now).unwrap().is_none());
}

#[test]
fn a_held_request_the_machine_answered_with_an_error_was_refused() {
    use shoal_protocol::protocol::{Body, ErrorCode, Response};
    let answered = |body: Body| {
        let frames = vec![serde_json::to_value(Response::new(1, body)).unwrap()];
        Reply::Answered(shoal_protocol::hub::routing::Answered { frames })
    };
    let refused = answered(Body::error(ErrorCode::UnprovenStop, "it may still run"));
    assert_eq!(refusal(&refused).as_deref(), Some("it may still run"));
    assert_eq!(refusal(&answered(Body::Ok)), None);
}
