use super::*;
use crate::model::Chat;
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
        "checkout": {"state": "created", "base": {"commit": "abc"}, "git_dir": "/admin"},
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
        wake_at: None,
        asleep: None,
        restart: None,
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
fn a_target_resolves_to_a_space_by_id_whether_its_agent_runs() {
    let resolved = |selector: &str| {
        let selected = Selected::resolved(&spaces(), vec![SpaceSelector::from(selector)]);
        selected.spaces.unwrap()
    };
    assert_eq!(resolved("3"), [SpaceSelector::id(&"w1".into())]);
    assert_eq!(resolved("c1"), [SpaceSelector::id(&"c1".into())]);
    assert_eq!(resolved("c2"), [SpaceSelector::id(&"c2".into())]);
    // A selector no space answers stays for the daemon to refuse.
    assert_eq!(resolved("missing"), [SpaceSelector::from("missing")]);
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
