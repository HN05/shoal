use super::*;

/// A view of a connected laptop's workspace with an agent and of a
/// workspace without one on a server that is offline.
fn view() -> HubView {
    let space = |machine: &str, id: &str, agent: bool| {
        serde_json::json!({
            "machine": machine, "id": id, "number": 1, "name": id, "kind": "workspace",
            "status": "working", "agent": agent, "reported_at": 0,
        })
    };
    serde_json::from_value(serde_json::json!({
        "machine": "m-desk",
        "machines": [
            {"id": "m-laptop", "kind": "machine", "name": "laptop", "joined_at": 0,
             "connected": true, "hub": false},
            {"id": "m-server", "kind": "machine", "name": "server", "joined_at": 0,
             "connected": false, "last_seen": 0, "hub": false},
        ],
        "spaces": [space("m-laptop", "work", true), space("m-server", "build", false)],
    }))
    .unwrap()
}

fn entries(need: Need) -> Entries {
    view_entries(
        &view(),
        Offered::Workspaces,
        need,
        Palette::stdout(true),
        None,
    )
}

#[test]
fn attach_offers_only_the_other_machines_spaces_with_an_agent() {
    let ids: Vec<String> = (entries(Need::Session).into_iter())
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids, ["work"]);
}

#[test]
fn each_offered_space_names_its_machine_and_an_offline_one_when_it_was_seen() {
    let [(_, work), (_, build)] = <[_; 2]>::try_from(entries(Need::Any)).unwrap();
    assert!(work.contains("laptop"), "{work}");
    assert!(!work.contains("seen"), "{work}");
    assert!(build.contains("server, seen"), "{build}");
}
