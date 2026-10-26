use super::*;

fn workspace(id: &str) -> Workspace {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "repository_id": "repo",
        "name": id,
        "path": format!("/{id}"),
        "branch": id,
        "state": "ready",
    }))
    .unwrap()
}

#[test]
fn previous_skips_the_current_and_missing_workspaces() {
    let workspaces = [workspace("a"), workspace("b")];
    let mut history = History::default();
    for id in ["a", "removed", "b"] {
        history.record("cd", id);
    }
    assert_eq!(history.0["cd"], ["b", "removed", "a"]);
    let previous = |current| history.previous("cd", &workspaces, current).map(|w| &w.id);
    assert_eq!(previous(None).unwrap(), "b");
    assert_eq!(previous(workspaces.get(1)).unwrap(), "a");
    assert!(history.previous("attach", &workspaces, None).is_none());
}

#[test]
fn recording_moves_a_workspace_to_the_front_and_keeps_the_limit() {
    let mut history = History::default();
    for id in 0..LIMIT + 2 {
        history.record("attach", &id.to_string());
    }
    history.record("attach", "5");
    let ids = &history.0["attach"];
    assert_eq!(ids.len(), LIMIT);
    assert_eq!(ids[0], "5");
    assert_eq!(ids.iter().filter(|id| *id == "5").count(), 1);
}
