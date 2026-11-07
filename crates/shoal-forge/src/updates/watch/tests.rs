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
    record.observe(snapshot(json!({}), "open"), "pr", 0);
    record.pending.clear();
    let comments = json!({"discussion:1": "one", "discussion:2": "two"});
    let updates = record.observe(snapshot(comments, "closed"), "pr", 0);
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
    let reopened = record.observe(snapshot(json!({}), "open"), "pr", 0);
    assert_eq!(reopened, updates);
    assert_eq!(record.snapshot.state(), Some(SnapshotState::Closed));
}

#[test]
fn own_changes_are_not_reported_back() {
    let mut record = WatchRecord::default();
    record.observe(snapshot(json!({"discussion:1": "theirs"}), "open"), "pr", 0);
    record.pending.clear();
    record.adopt_own_change(
        vec!["discussion:2".into()],
        Some(ItemState::Closed),
        &Edited::default(),
    );
    // A poll that read the item before the change keeps the adopted state.
    let stale = snapshot(json!({"discussion:1": "theirs"}), "open");
    assert!(record.observe(stale, "pr", 0).is_empty());
    let own = json!({"discussion:1": "theirs", "discussion:2": "mine"});
    assert!(record.observe(snapshot(own, "closed"), "pr", 0).is_empty());
    let next = json!({"discussion:1": "theirs", "discussion:2": "mine", "discussion:3": "new"});
    let updates = record.observe(snapshot(next, "closed"), "pr", u64::MAX);
    assert_eq!(
        updates.iter().map(|update| update.kind).collect::<Vec<_>>(),
        [UpdateKind::Comment]
    );
}
