use serde_json::{Value, json};

use super::*;
use crate::updates::{SnapshotState, UpdateKind};

fn snapshot(comments: Value, state: &str) -> Snapshot {
    serde_json::from_value(json!({"kind": "pr", "comments": comments, "checks": {},
            "conflict": null, "state": state, "errors": {}}))
    .unwrap()
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
fn a_conclusion_watch_reports_only_the_merge_or_close() {
    let mut record = WatchRecord::default();
    let first = record.observe_conclusion(
        snapshot(json!({"discussion:1": "one"}), "open"),
        "pr",
        UnixTime::EPOCH,
    );
    assert!(first.is_empty(), "{first:?}");
    let comments = json!({"discussion:1": "one", "discussion:2": "two"});
    assert!(
        (record.observe_conclusion(snapshot(comments.clone(), "open"), "pr", UnixTime::EPOCH))
            .is_empty()
    );
    let mut unreadable = snapshot(comments.clone(), "open");
    unreadable.errors.insert("item".into(), "not found".into());
    let failed = record.observe_conclusion(unreadable, "pr", UnixTime::EPOCH);
    assert_eq!(
        failed.iter().map(|update| update.kind).collect::<Vec<_>>(),
        [UpdateKind::LookupFailed]
    );
    record.pending.clear();
    let merged = record.observe_conclusion(snapshot(comments, "merged"), "pr", UnixTime::EPOCH);
    assert_eq!(
        merged.iter().map(|update| update.kind).collect::<Vec<_>>(),
        [UpdateKind::Merged]
    );
}
