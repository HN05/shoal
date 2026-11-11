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
    let chat = |id: &str, number: u32, name: &str, restore| Chat {
        narrowing: Default::default(),
        mode: "chat".into(),
        number: number.into(),
        id: id.into(),
        repository_id: Some("repo".into()),
        name: name.into(),
        agent: "claude".into(),
        workspace: None,
        pid: 1,
        started_at: UnixTime::from_seconds(0),
        origin: None,
        tmp_dir: None,
        root_dir: None,
        session: None,
        restore,
        links: Default::default(),
        review: Vec::new(),
        completion: None,
        agent_state: None,
        progress: None,
        parent: None,
        children: Vec::new(),
        converted: false,
    };
    Spaces {
        workspaces: vec![workspace],
        chats: vec![
            chat("c1", 1, "running", None),
            chat("c2", 2, "talk", Some(crate::state::ChatRestore::Resume)),
        ],
    }
}

#[test]
fn a_target_resolves_to_a_saved_chat_or_a_workspace() {
    let spaces = spaces();
    let resolved = |selector: &str| resolve(&spaces, SpaceSelector::from(selector)).unwrap();
    assert_eq!(
        resolved("3"),
        Target::Workspace(SpaceSelector::id(&"w1".into()))
    );
    assert!(matches!(resolved("c2"), Target::Chat(chat) if chat.name == "talk"));
    // A selector no space answers stays for the daemon to refuse.
    assert_eq!(resolved("missing"), Target::Workspace("missing".into()));
    assert!(resolve(&spaces, "c1".into()).is_err());
}

#[test]
fn a_space_named_twice_is_selected_once() {
    let selectors = ["c2", "3", "2", "w1"].map(SpaceSelector::from).to_vec();
    let selected = Selected::resolved(&spaces(), selectors).unwrap();
    assert_eq!(selected.chats.len(), 1);
    assert_eq!(
        selected.workspaces,
        Some(vec![SpaceSelector::id(&"w1".into())])
    );
}

#[test]
fn only_saved_chats_are_offered() {
    let saved = saved_chats(&spaces());
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "talk");
}
