use std::ffi::OsString;

use super::*;

#[test]
fn a_chat_started_beside_keeps_its_agent_model_and_arguments() {
    let topic = Topic {
        relaunch: vec!["chat".into(), "repo".into()],
        name: "repo-chat".into(),
        asks_name: true,
        opening: None,
        directories: Vec::new(),
        links: Vec::new(),
        workspace: None,
    };
    let agent: Agent = "codex".parse().unwrap();
    let model = ModelChoice {
        model: None,
        effort: Some("high".into()),
    };
    let args = [OsString::from("--search")];
    let beside = Beside {
        topic: &topic,
        name: "named",
        agent: &agent,
        model: &model,
        args: &args,
        session: None,
        restore: None,
    };
    assert_eq!(
        beside.relaunch(None),
        [
            "chat", "repo", "--here", "--name", "named", "--agent", "codex", "--effort", "high",
            "--", "--search",
        ]
        .map(OsString::from)
    );
    // A chat continuing a session takes it to the place it opens, as a
    // restore takes the saved chat, and so does a scoped start its handoff.
    let resumed = Beside {
        session: Some("s1"),
        restore: Some("c1"),
        ..beside
    };
    assert_eq!(
        resumed.relaunch(Some(Path::new("/tmp/shoal-chat.1"))),
        [
            "chat",
            "repo",
            "--here",
            "--name",
            "named",
            "--agent",
            "codex",
            "--effort",
            "high",
            "--session",
            "s1",
            "--restore",
            "c1",
            "--handoff",
            "/tmp/shoal-chat.1",
            "--",
            "--search",
        ]
        .map(OsString::from)
    );
}

#[test]
fn a_chat_in_a_workspace_reopens_there_and_is_told_not_to_edit() {
    let mut workspace = Workspace::new_record(
        "repo".into(),
        "worker".into(),
        "/work/worker".into(),
        Some("feature".into()),
        crate::state::WorkspaceState::Ready,
    );
    workspace.id = "worker-id".into();
    let topic = Topic::workspace(workspace.clone());
    assert_eq!(topic.name, "worker-chat");
    let agent: Agent = "claude".parse().unwrap();
    let beside = Beside {
        topic: &topic,
        name: "worker-chat",
        agent: &agent,
        model: &ModelChoice::default(),
        args: &[],
        session: None,
        restore: None,
    };
    assert_eq!(
        beside.relaunch(None),
        [
            "chat",
            "--workspace",
            "worker-id",
            "--here",
            "--name",
            "worker-chat",
            "--agent",
            "claude",
        ]
        .map(OsString::from)
    );

    let repo = Repository {
        id: "repo".into(),
        path: "/repos/shoal".into(),
        source: "/repos/shoal".into(),
        last_used: 0,
        name: Some("shoal".into()),
        workspaces_dir: None,
        contribution: false,
    };
    assert_eq!(topic.home(&repo).path(), Path::new("/work/worker"));
    let settings = crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap();
    let told = instructions(&settings, &agent, &repo, topic.home(&repo), false);
    for expected in [
        "workspace worker of the shoal",
        "branch feature",
        "/work/worker",
        "`--space worker`",
    ] {
        assert!(told.contains(expected), "{expected:?} in {told}");
    }
    // The chat template describes the checkout, which the chat is not in.
    assert!(!told.contains("registered\ncheckout"), "{told}");

    workspace.branch = None;
    workspace.at = Some(crate::model::StartingCommit {
        commit: "0123456789abcdef".into(),
        made_for: None,
    });
    let at_commit = Topic::workspace(workspace);
    let told = instructions(&settings, &agent, &repo, at_commit.home(&repo), false);
    assert!(told.contains("at commit 0123456"), "{told}");
}

#[test]
fn a_chat_is_refused_a_workspace_that_is_not_ready_before_anything_is_asked() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::new_record(
        "repo".into(),
        "worker".into(),
        root.path().to_owned(),
        Some("worker".into()),
        crate::state::WorkspaceState::Ready,
    );
    ensure_hosts_chat(&workspace).unwrap();
    workspace.state = crate::state::WorkspaceState::Failed;
    let failed = ensure_hosts_chat(&workspace).unwrap_err();
    assert!(failed.to_string().contains("worker is failed"), "{failed}");
    workspace.state = crate::state::WorkspaceState::Ready;
    workspace.path = root.path().join("gone");
    let missing = ensure_hosts_chat(&workspace).unwrap_err();
    assert!(missing.to_string().contains("is missing"), "{missing}");
}
