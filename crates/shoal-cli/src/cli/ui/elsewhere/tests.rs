use super::*;

/// A view of a connected laptop's workspace, running chat and stopped
/// chat, and of a workspace on a server that is offline.
fn view() -> HubView {
    let space = |machine: &str, id: &str, kind: &str, running: Option<&str>| {
        serde_json::json!({
            "machine": machine, "id": id, "number": 1, "name": id, "kind": kind,
            "status": "working", "running": running, "reported_at": 0,
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
        "spaces": [
            space("m-laptop", "work", "workspace", None),
            space("m-laptop", "talk", "chat", Some("claude")),
            space("m-laptop", "quiet", "chat", None),
            space("m-server", "build", "workspace", None),
        ],
    }))
    .unwrap()
}

fn offered_ids(offered: Offered) -> Vec<String> {
    let entries = view_entries(&view(), offered, Palette::stdout(true), None);
    entries.into_iter().map(|(id, _)| id).collect()
}

#[test]
fn pickers_offer_the_other_machines_spaces_of_the_kinds_they_pick() {
    assert_eq!(offered_ids(Offered::Workspaces), ["work", "build"]);
    assert_eq!(
        offered_ids(Offered::RunningChats),
        ["work", "talk", "build"]
    );
    assert_eq!(
        offered_ids(Offered::Spaces),
        ["work", "talk", "quiet", "build"]
    );
}

#[test]
fn each_offered_space_names_its_machine_and_an_offline_one_when_it_was_seen() {
    let entries = view_entries(&view(), Offered::Workspaces, Palette::stdout(true), None);
    let [(_, work), (_, build)] = <[_; 2]>::try_from(entries).unwrap();
    assert!(work.contains("laptop"), "{work}");
    assert!(!work.contains("seen"), "{work}");
    assert!(build.contains("server, seen"), "{build}");
}
