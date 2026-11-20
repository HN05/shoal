use serde_json::json;
use shoal_protocol::hub::directory::Listed;

use super::*;

fn listed(id: &str, parent: Option<&str>) -> Listed {
    serde_json::from_value(json!({
        "machine": "laptop", "id": id, "number": 1, "name": id, "kind": "chat",
        "status": "working", "parent": parent, "reported_at": 0,
    }))
    .unwrap()
}

fn chain(spaces: Vec<Listed>, first: &str) -> Vec<String> {
    let directory = Directory {
        spaces,
        ..Directory::default()
    };
    (chain_from(&directory, SpaceId::from(first.to_owned())).into_iter())
        .map(|id| id.to_string())
        .collect()
}

#[test]
fn the_spaces_above_go_on_through_the_directory_to_the_top() {
    let spaces = vec![
        listed("coordinator", Some("top")),
        listed("top", None),
        listed("aside", Some("top")),
    ];
    assert_eq!(chain(spaces, "coordinator"), ["coordinator", "top"]);
}

#[test]
fn the_spaces_above_stop_at_one_the_directory_does_not_list() {
    assert_eq!(chain(Vec::new(), "gone"), ["gone"]);
    let spaces = vec![listed("parent", Some("unlisted"))];
    assert_eq!(chain(spaces, "parent"), ["parent", "unlisted"]);
}

#[test]
fn the_spaces_above_end_on_a_cycle() {
    let spaces = vec![listed("one", Some("two")), listed("two", Some("one"))];
    assert_eq!(chain(spaces, "one").len(), 3);
}
