use super::*;

fn kinds(entries: &[LogEntry]) -> Vec<EventKind> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            LogEntry::Event(event) => Some(event.details.kind()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_restore_shows_as_restored_rather_than_created_again() {
    let event = |id, kind: &str| {
        serde_json::from_value::<LogEntry>(serde_json::json!({
            "type": "event", "id": id, "created_at": 1, "kind": kind, "space_id": "w",
            "space_kind": "workspace",
            "repository_id": "r", "name": "fix", "path": "/fix", "branch": "fix",
            "cause": null, "error": null,
        }))
        .unwrap()
    };
    let events = vec![
        event(1, "created"),
        event(2, "removed"),
        event(3, "created"),
        event(4, "ready"),
        event(5, "restored"),
    ];
    assert_eq!(
        kinds(&shown(events.clone(), LogFilter::Entries, 50)),
        [EventKind::Created, EventKind::Removed, EventKind::Restored]
    );
    assert_eq!(shown(events, LogFilter::All, 50).len(), 5);
}
