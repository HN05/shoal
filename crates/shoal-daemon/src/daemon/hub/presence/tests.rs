use std::time::Duration;

use rusqlite::Connection;

use super::*;

fn database() -> Connection {
    let db = shoal_store::test_support::migrated().unwrap();
    db.execute_batch(
        "INSERT INTO hub_members(id, kind, name, public_key, joined_at)
         VALUES ('laptop','machine','laptop','k1',0);",
    )
    .unwrap();
    db
}

fn laptop() -> MemberId {
    MemberId::from("laptop".to_owned())
}

fn at(minutes: u64) -> UnixTime {
    UnixTime::from_seconds(1_000_000) + Duration::from_secs(minutes * 60)
}

fn mark_at(db: &mut Connection, minutes: u64) {
    let tx = db.transaction().unwrap();
    mark(&tx, &laptop(), at(minutes)).unwrap();
    tx.commit().unwrap();
}

fn history_at(db: &mut Connection, minutes: u64) -> History {
    let tx = db.transaction().unwrap();
    history(&tx, &laptop(), at(minutes)).unwrap()
}

fn span(start: u64, end: u64) -> Span {
    Span {
        start: at(start),
        end: at(end),
    }
}

#[test]
fn a_gap_shorter_than_five_minutes_extends_a_span_and_one_of_five_starts_another() {
    let mut db = database();
    for minute in [0, 1, 2, 6, 20, 21, 26] {
        mark_at(&mut db, minute);
    }
    let history = history_at(&mut db, 26);
    assert_eq!(history.since, Some(at(0)));
    // An absence of exactly five minutes counts.
    assert_eq!(history.spans, [span(0, 6), span(20, 21), span(26, 26)]);
}

#[test]
fn spans_that_ended_a_week_ago_go() {
    let mut db = database();
    mark_at(&mut db, 0);
    let week = KEPT.as_secs() / 60;
    mark_at(&mut db, week + 10);
    let tx = db.transaction().unwrap();
    prune(&tx, at(week + 10)).unwrap();
    tx.commit().unwrap();
    let history = history_at(&mut db, week + 10);
    assert_eq!(history.spans, [span(week + 10, week + 10)]);
    // When the hub first saw the machine stays.
    assert_eq!(history.since, Some(at(0)));
}
