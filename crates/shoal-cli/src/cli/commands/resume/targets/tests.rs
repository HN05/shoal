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
    let resolved = |selector: &str| resolved(&spaces(), vec![SpaceSelector::from(selector)]);
    assert_eq!(resolved("3"), [SpaceSelector::id(&"w1".into())]);
    assert_eq!(resolved("c1"), [SpaceSelector::id(&"c1".into())]);
    assert_eq!(resolved("c2"), [SpaceSelector::id(&"c2".into())]);
    // A selector no space answers stays for the daemon to refuse.
    assert_eq!(resolved("missing"), [SpaceSelector::from("missing")]);
}

#[test]
fn a_space_named_twice_is_selected_once() {
    let selectors = ["c2", "3", "2", "w1"].map(SpaceSelector::from).to_vec();
    assert_eq!(
        resolved(&spaces(), selectors),
        [
            SpaceSelector::id(&"c2".into()),
            SpaceSelector::id(&"w1".into())
        ]
    );
}

/// A view of a desk's workspaces, `saved` with stopped work and `launch`
/// with only a failed launch.
fn view() -> HubView {
    let space = |id: &str, number: u32, resumable: &str| {
        serde_json::json!({
            "machine": "m-desk", "id": id, "number": number, "name": id, "kind": "workspace",
            "status": "stopped", "resumable": resumable, "reported_at": 0,
        })
    };
    serde_json::from_value(serde_json::json!({
        "machine": "m-laptop",
        "machines": [
            {"id": "m-desk", "kind": "machine", "name": "desk", "joined_at": 0,
             "connected": true, "hub": false},
        ],
        "spaces": [space("saved", 7, "saved"), space("launch", 8, "failed_launch")],
    }))
    .unwrap()
}

#[test]
fn a_space_another_machine_has_is_targeted_there_by_its_id() {
    let named = Target::named(SpaceSelector::from("7"), Some(&view()));
    assert_eq!(named.selector, SpaceSelector::id(&"saved".into()));
    assert_eq!(named.machine_name().as_deref(), Some("desk"));
    assert_eq!(named.shown(), "saved on desk");
    let unknown = Target::named(SpaceSelector::from("9"), Some(&view()));
    assert_eq!(unknown.selector, SpaceSelector::from("9"));
    assert_eq!(unknown.machine, None);
}

#[test]
fn a_machine_s_spaces_resume_only_with_saved_work_and_discard_with_a_failed_launch_too() {
    let view = view();
    let taken = |taking: Taking| -> Vec<_> {
        (view.directory.spaces.iter())
            .filter(|listed| taking.takes(listed))
            .map(|listed| listed.space.id.to_string())
            .collect()
    };
    assert_eq!(taken(Taking::Resumes), ["saved"]);
    assert_eq!(taken(Taking::Discards), ["saved", "launch"]);
}
