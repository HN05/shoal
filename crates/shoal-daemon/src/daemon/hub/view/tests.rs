use serde_json::json;

use super::*;

fn listed(id: &str, parent: Option<&str>) -> Listed {
    serde_json::from_value(json!({
        "machine": "laptop", "id": id, "number": 1, "name": id, "kind": "chat",
        "status": "working", "parent": parent, "reported_at": 0,
    }))
    .unwrap()
}

#[test]
fn a_scope_reaches_the_spaces_below_it_through_other_machines() {
    let spaces = vec![
        listed("child", Some("mine")),
        listed("grandchild", Some("child")),
        listed("elsewhere", Some("other")),
        listed("top", None),
        listed("loop", Some("loop")),
    ];
    let reached = [SpaceId::from("mine".to_owned())].into();
    let kept: Vec<String> = (below(spaces, &reached).into_iter())
        .map(|listed| listed.space.id.to_string())
        .collect();
    assert_eq!(kept, ["child", "grandchild"]);
}

#[test]
fn a_scope_sees_only_the_conflicts_over_the_spaces_it_reaches() {
    let conflict = |space: &str| -> shoal_protocol::hub::directory::Conflict {
        serde_json::from_value(json!({
            "space": space, "name": space, "owner": "laptop", "copy": "desk", "since": 0,
        }))
        .unwrap()
    };
    let mut directory = Directory {
        machines: Vec::new(),
        spaces: vec![listed("child", Some("mine")), listed("other", None)],
        conflicts: vec![conflict("mine"), conflict("child"), conflict("other")],
    };
    let reached = [SpaceId::from("mine".to_owned())].into();
    within_reach(&mut directory, &reached);
    let kept: Vec<String> = (directory.conflicts.iter())
        .map(|conflict| conflict.space.to_string())
        .collect();
    assert_eq!(kept, ["mine", "child"]);
}
