use serde_json::json;

use super::*;

/// A view of a laptop's workspace, running, sleeping and stopped chats, and
/// of a server's workspace that shares the workspace's number.
fn view() -> HubView {
    let space = |machine: &str, id: &str, number: u32, kind: &str, running: Option<&str>| {
        let status = if id == "nap" { "asleep" } else { "working" };
        json!({
            "machine": machine, "id": id, "number": number, "name": id, "kind": kind,
            "status": status, "running": running, "reported_at": 0,
        })
    };
    serde_json::from_value(json!({
        "machine": "m-desk",
        "machines": [
            {"id": "m-laptop", "kind": "machine", "name": "laptop", "joined_at": 0,
             "connected": true, "hub": false},
        ],
        "spaces": [
            space("m-laptop", "work", 1, "workspace", None),
            space("m-laptop", "talk", 2, "chat", Some("claude")),
            space("m-laptop", "quiet", 3, "chat", None),
            space("m-laptop", "nap", 4, "chat", None),
            space("m-server", "build", 1, "workspace", None),
        ],
    }))
    .unwrap()
}

fn ids<'a>(spaces: impl Iterator<Item = &'a Listed>) -> Vec<&'a str> {
    spaces.map(|listed| listed.space.id.as_str()).collect()
}

#[test]
fn pickers_offer_the_other_machines_spaces_of_the_kinds_they_pick() {
    let view = view();
    let offered = |offered: Offered| {
        ids((view.directory.spaces.iter()).filter(|listed| offered.offers(&listed.space)))
    };
    assert_eq!(offered(Offered::Workspaces), ["work", "build"]);
    assert_eq!(
        offered(Offered::RunningChats),
        ["work", "talk", "nap", "build"]
    );
    assert_eq!(
        offered(Offered::Spaces),
        ["work", "talk", "quiet", "nap", "build"]
    );
}

#[test]
fn attach_returns_to_workspaces_and_chats_whose_agent_runs_or_sleeps() {
    let attachable = view().attachable();
    let attachable: Vec<&str> = attachable.iter().map(SpaceId::as_str).collect();
    assert_eq!(attachable, ["work", "talk", "nap", "build"]);
}

#[test]
fn a_number_selects_a_space_only_one_other_machine_has() {
    let view = view();
    let selected = |text: &str| view.selected(&SpaceSelector::parse(text).unwrap());
    let id = |listed: Option<&Listed>| listed.map(|listed| listed.space.id.to_string());
    assert_eq!(id(selected("2").unwrap()), Some("talk".to_owned()));
    let build = SpaceSelector::id(&SpaceId::from("build".to_owned()));
    assert_eq!(id(view.selected(&build).unwrap()), Some("build".to_owned()));
    assert!(selected("1").is_err());
    assert!(selected("9").unwrap().is_none());
}

#[test]
fn a_listed_space_names_its_machine_or_else_its_id() {
    let view = view();
    let name = |id: &str| view.machine_name(view.listed(&SpaceId::from(id.to_owned())).unwrap());
    assert_eq!(name("work"), "laptop");
    assert_eq!(name("build"), "m-server");
}
