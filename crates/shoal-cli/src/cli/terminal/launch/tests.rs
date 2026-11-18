use super::*;

#[test]
fn placement_follows_the_setting_then_whether_the_launch_waits() {
    assert_eq!(Placement::choose(None, true), Placement::Focus);
    assert_eq!(Placement::choose(None, false), Placement::Background);
    assert_eq!(Placement::choose(Some(false), true), Placement::Background);
    assert_eq!(Placement::choose(Some(true), false), Placement::Focus);
}

/// A chat, or with `fields` another space, as the daemon describes it.
fn space(fields: serde_json::Value) -> Space {
    let mut space = serde_json::json!({
        "id": "0b9c3a52-7d1e-4a7f-9c1d-2f8e6b4a1c3d",
        "number": 4,
        "name": "Fix the login",
        "kind": "chat",
        "started_at": 0,
    });
    space
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(space).unwrap()
}

#[test]
fn a_workspace_agent_runs_in_its_worktree_in_a_session_named_after_it() {
    let workspace = space(serde_json::json!({
        "kind": "workspace",
        "repository_id": "repo",
        "workdir": {"kind": "worktree", "path": "/work/fix-login", "workspace": {"id": "w1", "name": "worker", "branch": "fix-login"}},
        "path": "/work/fix-login",
        "branch": "fix-login",
        "state": "ready",
    }));
    let placed = AgentSpace::of(&workspace);
    assert!(matches!(placed.repo, groups::Repo::Id("repo")));
    assert_eq!(placed.directory, Path::new("/work/fix-login"));
    assert_eq!(placed.label, "Fix the login");
    assert_eq!(placed.session, "fix-login");
    assert_eq!(placed.owner, Owner::Workspace(&workspace.id));
}

#[test]
fn a_chat_agent_runs_in_its_workdir_in_a_session_named_after_its_id() {
    let chat = space(serde_json::json!({
        "repository_id": "repo",
        "workdir": {"kind": "checkout", "path": "/repos/repo"},
    }));
    let placed = AgentSpace::of(&chat);
    assert!(matches!(placed.repo, groups::Repo::Id("repo")));
    assert_eq!(placed.directory, Path::new("/repos/repo"));
    assert_eq!(placed.label, "Fix the login");
    assert_eq!(placed.session, "chat-0b9c3a52");
    assert_eq!(placed.owner, Owner::Chat(&chat.id));
}

#[test]
fn a_chat_without_a_repository_opens_beside_its_root_dir() {
    let chat = space(serde_json::json!({
        "workdir": {"kind": "root_dir", "path": "/home/user/shoal"},
    }));
    let placed = AgentSpace::of(&chat);
    assert!(
        matches!(placed.repo, groups::Repo::Root(root) if root == Path::new("/home/user/shoal"))
    );
}
