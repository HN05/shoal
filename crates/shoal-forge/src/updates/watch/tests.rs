use serde_json::{Value, json};

use super::*;
use shoal_forge_types::updates::SnapshotState;
use shoal_forge_types::updates::UpdateKind;

/// A snapshot of a PR in `state` whose comments have the fingerprints
/// `comments` holds.
fn snapshot(comments: Value, state: &str) -> Snapshot {
    serde_json::from_value(
        json!({"kind": "pr", "comments": entries(comments), "checks": {},
            "conflict": null, "state": state, "errors": {}}),
    )
    .unwrap()
}

/// Each fingerprint of `comments` as an entry without an author.
fn entries(comments: Value) -> Value {
    let entries = comments.as_object().into_iter().flatten();
    Value::Object(
        entries
            .map(|(key, fingerprint)| (key.clone(), json!({"fingerprint": fingerprint})))
            .collect(),
    )
}

#[test]
fn new_updates_become_one_delivery_reported_until_acknowledged() {
    let mut record = WatchRecord::default();
    record.observe(snapshot(json!({}), "open"), "pr", UnixTime::EPOCH);
    record.pending.clear();
    let comments = json!({"discussion:1": "one", "discussion:2": "two"});
    let updates = record.observe(snapshot(comments, "closed"), "pr", UnixTime::EPOCH);
    assert_eq!(
        updates.iter().map(|update| update.kind).collect::<Vec<_>>(),
        [UpdateKind::Comment, UpdateKind::Closed]
    );
    assert!(!updates[0].delivery.is_empty());
    assert!(
        updates
            .iter()
            .all(|update| update.delivery == updates[0].delivery)
    );
    // Until the caller clears them, the pending updates are reported again
    // and the next observation waits.
    let reopened = record.observe(snapshot(json!({}), "open"), "pr", UnixTime::EPOCH);
    assert_eq!(reopened, updates);
    assert_eq!(record.snapshot.state(), Some(SnapshotState::Closed));
}

#[test]
fn own_changes_are_not_reported_back() {
    let mut record = WatchRecord::default();
    record.observe(
        snapshot(json!({"discussion:1": "theirs"}), "open"),
        "pr",
        UnixTime::EPOCH,
    );
    record.pending.clear();
    record.adopt_own_change(
        vec!["discussion:2".into()],
        Some(ItemState::Closed),
        &Edited::default(),
    );
    // A poll that read the item before the change keeps the adopted state.
    let stale = snapshot(json!({"discussion:1": "theirs"}), "open");
    assert!(record.observe(stale, "pr", UnixTime::EPOCH).is_empty());
    let own = json!({"discussion:1": "theirs", "discussion:2": "mine"});
    assert!(
        record
            .observe(snapshot(own, "closed"), "pr", UnixTime::EPOCH)
            .is_empty()
    );
    let next = json!({"discussion:1": "theirs", "discussion:2": "mine", "discussion:3": "new"});
    let updates = record.observe(snapshot(next, "closed"), "pr", UnixTime::MAX);
    assert_eq!(
        updates.iter().map(|update| update.kind).collect::<Vec<_>>(),
        [UpdateKind::Comment]
    );
}

#[test]
fn a_start_reports_the_condition_of_an_item_never_read_but_not_its_comments() {
    let mut record = WatchRecord::default();
    let earlier = json!({"discussion:1": "one", "discussion:2": "two"});
    let read_at = UnixTime::from_seconds(10);
    record.start_from(snapshot(earlier, "closed"), "pr", read_at);
    assert_eq!(
        record
            .pending
            .iter()
            .map(|update| update.kind)
            .collect::<Vec<_>>(),
        [UpdateKind::Closed]
    );
    record.pending.clear();
    // A poll that started before the read may show the item before it.
    let stale = snapshot(json!({"discussion:1": "one"}), "closed");
    let before = UnixTime::from_seconds(9);
    assert!(record.observe(stale, "pr", before).is_empty());
    let later = json!({"discussion:1": "one", "discussion:2": "two", "discussion:3": "three"});
    let updates = record.observe(snapshot(later, "closed"), "pr", UnixTime::MAX);
    assert_eq!(
        updates[0].message,
        "1 new comment; read them with shoal view pr"
    );
}

#[test]
fn a_restart_keeps_only_the_queued_updates_of_the_items_condition() {
    let mut record = WatchRecord::default();
    record.observe(snapshot(json!({}), "open"), "pr", UnixTime::EPOCH);
    record.pending.clear();
    let queued = json!({"discussion:1": "one"});
    let updates = record.observe(snapshot(queued, "closed"), "pr", UnixTime::EPOCH);
    assert_eq!(updates.len(), 2);
    let read = json!({"discussion:1": "one", "discussion:2": "two"});
    record.start_from(snapshot(read, "closed"), "pr", UnixTime::EPOCH);
    // The queued comment goes; the item's closing stays, reported once.
    assert_eq!(
        record
            .pending
            .iter()
            .map(|update| update.kind)
            .collect::<Vec<_>>(),
        [UpdateKind::Closed]
    );
}

#[test]
fn a_restart_reports_a_condition_its_read_first_shows() {
    let mut record = WatchRecord::default();
    record.observe(snapshot(json!({}), "open"), "pr", UnixTime::EPOCH);
    record.pending.clear();
    record.start_from(snapshot(json!({}), "closed"), "pr", UnixTime::EPOCH);
    assert_eq!(
        record
            .pending
            .iter()
            .map(|update| update.kind)
            .collect::<Vec<_>>(),
        [UpdateKind::Closed]
    );
}

#[test]
fn a_poll_that_started_before_the_start_stays_stale_after_a_newer_one() {
    let mut record = WatchRecord::default();
    let read = json!({"discussion:1": "one", "discussion:2": "two"});
    let read_at = UnixTime::from_seconds(10);
    record.start_from(snapshot(read.clone(), "open"), "pr", read_at);
    let newer = UnixTime::from_seconds(11);
    assert!(
        record
            .observe(snapshot(read, "open"), "pr", newer)
            .is_empty()
    );
    let stale = snapshot(json!({"discussion:1": "one"}), "open");
    let older = UnixTime::from_seconds(9);
    assert!(record.observe(stale, "pr", older).is_empty());
    assert_eq!(record.snapshot.comments.len(), 2);
}
