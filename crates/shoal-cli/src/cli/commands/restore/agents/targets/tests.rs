use super::*;
use crate::client::space_names::SpaceSelector;
use crate::model::Workspace;
use crate::time::UnixTime;

fn spaces() -> Spaces {
    let workspace: Workspace = serde_json::from_value(serde_json::json!({
        "id": "w1",
        "number": 3,
        "repository_id": "repo",
        "name": "worker",
        "path": "/worker",
        "branch": "worker",
        "state": "ready",
    }))
    .unwrap();
    let chat = |id: &str, number: u32, name: &str, running| Chat {
        narrowing: Default::default(),
        mode: "chat".into(),
        number: number.into(),
        id: id.into(),
        repository_id: Some("repo".into()),
        name: name.into(),
        agent: "claude".into(),
        workspace: None,
        started_at: UnixTime::from_seconds(0),
        origin: None,
        tmp_dir: None,
        root_dir: None,
        session: None,
        running,
        links: Default::default(),
        review: Vec::new(),
        completion: None,
        agent_state: None,
        progress: None,
        sleep: None,
        parent: None,
        children: Vec::new(),
        converted: false,
    };
    Spaces {
        workspaces: vec![workspace],
        chats: vec![chat("c1", 1, "running", true), chat("c2", 2, "talk", false)],
    }
}

#[test]
fn a_target_resolves_to_a_running_chat_or_a_space_by_id() {
    let spaces = spaces();
    let resolved = |selector: &str| resolve(&spaces, SpaceSelector::from(selector));
    assert_eq!(
        resolved("3"),
        Target::Space(SpaceSelector::id(&"w1".into()))
    );
    assert_eq!(
        resolved("c2"),
        Target::Space(SpaceSelector::id(&"c2".into()))
    );
    // A selector no space answers stays for the daemon to refuse.
    assert_eq!(resolved("missing"), Target::Space("missing".into()));
    assert!(matches!(resolved("c1"), Target::Running(chat) if chat.name == "running"));
}

#[test]
fn a_space_named_twice_is_selected_once() {
    let selectors = ["c2", "3", "2", "w1"].map(SpaceSelector::from).to_vec();
    let selected = Selected::resolved(&spaces(), selectors);
    assert_eq!(
        selected.spaces,
        Some(vec![
            SpaceSelector::id(&"c2".into()),
            SpaceSelector::id(&"w1".into())
        ])
    );
}

#[test]
fn only_stopped_chats_are_offered() {
    let stopped = stopped_chats(&spaces());
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0].name, "talk");
}
