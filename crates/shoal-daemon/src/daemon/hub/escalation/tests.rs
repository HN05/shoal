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

#[test]
fn a_decider_elsewhere_that_lost_its_agent_or_went_passes_its_requests_on() {
    let machine = |id: &str, connected: bool| {
        json!({
            "id": id, "kind": "machine", "name": id, "version": null, "joined_at": 0,
            "last_seen": 100, "connected": connected, "hub": false,
        })
    };
    let listed = |id: &str, machine: &str, agent: bool| {
        json!({
            "machine": machine, "id": id, "number": 1, "name": id, "kind": "chat",
            "status": "working", "agent": agent, "reported_at": 0,
        })
    };
    let directory: Directory = serde_json::from_value(json!({
        "machines": [machine("desk", true), machine("laptop", false), machine("here", true)],
        "spaces": [
            listed("working", "desk", true),
            listed("stopped", "desk", false),
            listed("away", "laptop", true),
            listed("mine", "here", true),
        ],
    }))
    .unwrap();
    let own = shoal_ids::member_ids::MemberId::from("here".to_owned());
    let deciders = Deciders {
        own: &own,
        directory: &directory,
        now: UnixTime::from_seconds(1000),
        after: Duration::from_secs(600),
    };
    let unavailable = |id: &str, created: i64| {
        deciders.unavailable(
            &SpaceId::from(id.to_owned()),
            UnixTime::from_seconds(created),
        )
    };
    assert_eq!(unavailable("working", 0), None);
    assert_eq!(unavailable("mine", 0), None);
    assert_eq!(unavailable("stopped", 990), Some(Unavailable::Gone));
    assert_eq!(unavailable("removed", 990), Some(Unavailable::Gone));
    // Offline since 100: a request waits from then, or from when it came.
    assert_eq!(unavailable("away", 0), Some(Unavailable::Offline));
    assert_eq!(unavailable("away", 500), None);
}
