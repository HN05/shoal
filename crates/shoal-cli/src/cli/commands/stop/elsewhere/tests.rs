use serde_json::json;

use super::*;

fn listed(id: &str, parent: Option<&str>) -> serde_json::Value {
    json!({
        "machine": "laptop", "id": id, "number": 1, "name": id, "kind": "chat",
        "status": "working", "parent": parent, "reported_at": 0,
    })
}

#[test]
fn a_remote_stop_takes_the_spaces_below_on_every_machine_once() {
    let view: HubView = serde_json::from_value(json!({
        "machine": "desk",
        "machines": [],
        "spaces": [
            listed("top", None),
            listed("child", Some("top")),
            listed("grandchild", Some("child")),
            listed("aside", None),
            listed("loop", Some("loop")),
        ],
    }))
    .unwrap();
    let ids: Vec<String> = (below(&view, &SpaceId::from("top".to_owned())).into_iter())
        .map(|listed| listed.space.id.to_string())
        .collect();
    assert_eq!(ids, ["child", "grandchild"]);
}
