use super::*;
use crate::model::Workspace;

fn spaces() -> Spaces {
    let workspace: Workspace = serde_json::from_value(serde_json::json!({
        "id": "w1",
        "repository_id": "repo",
        "name": "worker",
        "path": "/worker",
        "branch": "worker",
        "state": "ready",
    }))
    .unwrap();
    let chat = |id: &str, name: &str, restore| Chat {
        id: id.into(),
        repository_id: "repo".into(),
        name: name.into(),
        agent: "claude".into(),
        pid: 1,
        started_at: 0,
        origin: None,
        tmp_dir: None,
        session: None,
        restore,
        links: Default::default(),
        review: Vec::new(),
    };
    Spaces {
        workspaces: vec![workspace],
        chats: vec![
            chat("c1", "running", None),
            chat("c2", "talk", Some(crate::state::ChatRestore::Resume)),
        ],
    }
}

#[test]
fn a_target_resolves_to_a_saved_chat_or_a_workspace() {
    let spaces = spaces();
    let resolved = |selector: &str| resolve(&spaces, selector.to_owned()).unwrap();
    assert_eq!(resolved("worker"), Target::Workspace("w1".into()));
    assert!(matches!(resolved("talk"), Target::Chat(chat) if chat.id == "c2"));
    assert!(matches!(resolved("c2"), Target::Chat(chat) if chat.name == "talk"));
    // A name no space has stays for the daemon to refuse.
    assert_eq!(resolved("missing"), Target::Workspace("missing".into()));
    let error = resolve(&spaces, "running".into()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "chat running is running; nothing to resume"
    );
}

#[test]
fn a_space_named_twice_is_selected_once() {
    let selectors = ["talk", "worker", "c2", "w1"].map(String::from).to_vec();
    let selected = Selected::resolved(&spaces(), selectors).unwrap();
    assert_eq!(selected.chats.len(), 1);
    assert_eq!(selected.workspaces, Some(vec!["w1".to_owned()]));
}

#[test]
fn only_saved_chats_are_offered() {
    let saved = saved_chats(&spaces());
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "talk");
}
