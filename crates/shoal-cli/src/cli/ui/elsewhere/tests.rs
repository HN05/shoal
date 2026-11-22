use super::*;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::Offer;

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

/// The entries of `view()`'s spaces as `judged` judges them.
fn entries(judged: &[(&str, Offer)]) -> Entries {
    let judged = (judged.iter())
        .map(|(id, offer)| (SpaceId::from(*id), offer.clone()))
        .collect();
    let offers = Offers::new(judged, None);
    view_entries(
        &view(),
        Offered::Workspaces,
        &offers,
        Palette::stdout(true),
        None,
    )
}

/// The IDs of `entries`, in order.
fn ids(entries: Entries) -> Vec<String> {
    entries.into_iter().map(|(id, _)| id).collect()
}

#[test]
fn a_space_its_daemon_finds_without_what_the_request_acts_on_is_left_out() {
    assert_eq!(ids(entries(&[("work", Offer::Lacking)])), ["build"]);
}

#[test]
fn a_space_its_daemon_refuses_comes_last_with_its_reason() {
    let refused = Offer::Refused {
        reason: "workspace work is preparing".into(),
    };
    let entries = entries(&[("work", refused)]);
    let [(build, _), (work, row)] = <[_; 2]>::try_from(entries).unwrap();
    assert_eq!([build.as_str(), work.as_str()], ["build", "work"]);
    assert!(row.contains("workspace work is preparing"), "{row}");
}

#[test]
fn each_offered_space_names_its_machine_and_an_offline_one_when_it_was_seen() {
    let [(_, work), (_, build)] = <[_; 2]>::try_from(entries(&[])).unwrap();
    assert!(work.contains("laptop"), "{work}");
    assert!(!work.contains("seen"), "{work}");
    assert!(build.contains("server, seen"), "{build}");
}
