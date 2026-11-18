use serde_json::json;

use super::*;

#[test]
fn a_space_is_offline_since_its_machine_was_last_seen() {
    let machine = |id: &str, connected: bool, last_seen: Option<u64>| {
        json!({
            "id": id, "kind": "machine", "name": id, "version": null, "joined_at": 10,
            "last_seen": last_seen, "connected": connected, "hub": false,
        })
    };
    let listed = |id: &str, machine: &str| {
        json!({
            "machine": machine, "id": id, "number": 1, "name": id, "kind": "chat",
            "status": "working", "reported_at": 0,
        })
    };
    let directory: Directory = serde_json::from_value(json!({
        "machines": [
            machine("desk", true, Some(50)),
            machine("laptop", false, Some(40)),
            machine("phone", false, None),
        ],
        "spaces": [listed("here", "desk"), listed("away", "laptop"), listed("never", "phone")],
    }))
    .unwrap();
    let offline = offline_since(&directory);
    let since = |id: &str| offline.get(&SpaceId::from(id.to_owned())).copied();
    assert_eq!(since("here"), None);
    assert_eq!(since("away"), Some(UnixTime::from_seconds(40)));
    assert_eq!(since("never"), Some(UnixTime::from_seconds(10)));
}
