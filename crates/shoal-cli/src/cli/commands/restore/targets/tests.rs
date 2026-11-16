use super::*;
use crate::model::Workspace;
use crate::time::UnixTime;
use shoal_protocol::space_names::SpaceSelector;

fn spaces() -> Spaces {
    let workspace: Workspace = serde_json::from_value(serde_json::json!({
        "workdir": {"kind": "worktree", "path": "/worker", "workspace": {"id": "w1", "name": "worker", "branch": "worker"}},
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
        mode: Some("chat".into()),
        number: number.into(),
        id: id.into(),
        repository_id: Some("repo".into()),
        name: name.into(),
        running,
        links: Default::default(),
        review: Vec::new(),
        holds: Vec::new(),
        agent_state: None,
        progress: None,
        sleep: None,
        asleep: None,
        parent: None,
        children: Vec::new(),
        workdir: shoal_protocol::model::Workdir::Checkout {
            path: "/repo".into(),
        },
        part: shoal_protocol::model::ChatPart {
            agent: Some("claude".into()),
            started_at: UnixTime::from_seconds(0),
            tmp_dir: None,
            completion: None,
        },
    };
    Spaces(vec![
        workspace.into(),
        chat("c1", 1, "running", true).into(),
        chat("c2", 2, "talk", false).into(),
    ])
}

#[test]
fn a_target_resolves_to_a_running_chat_or_a_space_by_id() {
    let spaces = spaces();
    let resolved = |selector: &str| resolve(&spaces, &[], SpaceSelector::from(selector));
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
    let selected = Selected::resolved(&spaces(), &[], selectors);
    assert_eq!(
        selected.spaces,
        Some(vec![
            SpaceSelector::id(&"c2".into()),
            SpaceSelector::id(&"w1".into())
        ])
    );
}

#[test]
fn a_selection_of_running_chats_alone_restores_nothing_else() {
    // Only the running chat is named: its interrupted turn is all there is.
    let only_running = Selected::resolved(&spaces(), &[], vec!["c1".into()]);
    assert!(only_running.is_empty());
    // A space with stopped work beside it restores too.
    let with_space = Selected::resolved(&spaces(), &[], vec!["c1".into(), "3".into()]);
    assert!(!with_space.is_empty());
    // Every space with stopped work, for --all.
    assert!(!Selected::default().is_empty());
}

#[test]
fn only_stopped_chats_are_offered() {
    let stopped = stopped_chats(&spaces());
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0].name, "talk");
}
