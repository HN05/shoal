use super::*;

fn view() -> HubView {
    serde_json::from_value(serde_json::json!({
        "machine": "m-desk",
        "machines": [
            {"id": "m-desk", "kind": "machine", "name": "desk", "joined_at": 0,
             "connected": true, "hub": true},
            {"id": "m-laptop", "kind": "machine", "name": "laptop", "joined_at": 0,
             "connected": true, "hub": false},
        ],
        "spaces": [
            {"machine": "m-laptop", "id": "a", "number": 1, "name": "a", "kind": "chat",
             "status": "working", "reported_at": 0},
            {"machine": "m-laptop", "id": "b", "number": 2, "name": "b", "kind": "chat",
             "status": "working", "reported_at": 0},
        ],
    }))
    .unwrap()
}

#[test]
fn a_machine_named_twice_is_selected_once_with_every_space_it_runs() {
    let view = view();
    let named = ["laptop".to_owned(), "m-laptop".to_owned()];
    let [laptop] = <[Member; 1]>::try_from(distinct(&view, &named).unwrap()).unwrap();
    let spaces: Vec<String> = (spaces_of(&view, &laptop).into_iter())
        .map(|(listed, _)| listed.space.id.to_string())
        .collect();
    assert_eq!(spaces, ["a", "b"]);
    assert!(distinct(&view, &["server".to_owned()]).is_err());
}
