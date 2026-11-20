use serde_json::json;

use super::*;

/// A member `id` named `name` of `kind`, connected or last seen at `seen`.
fn member(id: &str, name: &str, kind: &str, seen: Option<i64>) -> serde_json::Value {
    json!({
        "id": id, "kind": kind, "name": name, "joined_at": 0,
        "last_seen": seen, "connected": seen.is_none(), "hub": false,
    })
}

/// The hub's view from the machine `here`, with `machines`.
fn view(machines: Vec<serde_json::Value>) -> HubView {
    serde_json::from_value(json!({"machine": "here", "machines": machines, "spaces": []})).unwrap()
}

#[test]
fn the_other_machines_online_are_picked_by_name_and_the_offline_ones_listed() {
    let view = view(vec![
        member("here", "laptop", "machine", None),
        member("3", "server", "machine", None),
        member("2", "desk", "machine", None),
        member("4", "phone", "device", None),
        member("5", "old", "machine", Some(0)),
    ]);
    let (online, offline) = other_machines(&view);
    let names = |machines: &[&Member]| -> Vec<String> {
        machines
            .iter()
            .map(|machine| machine.name.clone())
            .collect()
    };
    assert_eq!(names(&online), ["desk", "server"]);
    assert!(can_pick_machine(&view));
    assert_eq!(names(&offline), ["old"]);
    let header = offline_header(&offline, UnixTime::from_seconds(0)).unwrap();
    assert!(header.contains("old (seen"), "{header}");
    assert_eq!(offline_header(&[], UnixTime::from_seconds(0)), None);
}

#[test]
fn a_machine_whose_name_names_another_is_named_by_its_id() {
    let view = view(vec![
        member("1", "desk", "machine", None),
        member("2", "desk", "machine", None),
        member("3", "server", "machine", None),
        member("4", "1", "machine", None),
        member("5", "server", "device", None),
    ]);
    assert_eq!(named(&view, "2"), "2");
    // A device of the same name leaves the machine's name its own.
    assert_eq!(named(&view, "3"), "server");
    // A name that is another machine's ID names that machine.
    assert_eq!(named(&view, "4"), "4");
}

#[test]
fn no_machine_is_offered_while_every_other_one_is_offline() {
    let view = view(vec![
        member("here", "laptop", "machine", None),
        member("5", "old", "machine", Some(0)),
    ]);
    assert!(!can_pick_machine(&view));
}
