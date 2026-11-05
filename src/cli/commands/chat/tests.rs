use std::ffi::OsString;

use super::*;

#[test]
fn a_chat_started_beside_keeps_its_agent_model_and_arguments() {
    let repo = Repository {
        id: "repo".into(),
        path: "/repos/shoal".into(),
        source: "/repos/shoal".into(),
        last_used: 0,
        name: Some("shoal".into()),
        workspaces_dir: None,
        contribution: false,
    };
    let topic = Topic::repository(&repo, CHAT_MODE);
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
        fork: false,
        restore: None,
        parent: None,
    };
    assert_eq!(
        beside.relaunch(None),
        [
            "start", "--chat", "--mode", "chat", "repo", "--here", "--name", "named", "--agent",
            "codex", "--effort", "high", "--", "--search",
        ]
        .map(OsString::from)
    );
    // A chat continuing a session takes it to the place it opens, as a
    // restore takes the saved chat, and so does a scoped start its handoff.
    // A chat below a named parent goes there too.
    let resumed = Beside {
        session: Some("s1"),
        restore: Some("c1"),
        parent: Some("7"),
        ..beside
    };
    assert_eq!(
        resumed.relaunch(Some(Path::new("/tmp/shoal-chat.1"))),
        [
            "start",
            "--chat",
            "--mode",
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
            "--parent",
            "7",
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
    let topic = Topic::workspace(workspace.clone(), "plan");
    assert_eq!(topic.name, "worker-chat");
    let agent: Agent = "claude".parse().unwrap();
    let beside = Beside {
        topic: &topic,
        name: "worker-chat",
        agent: &agent,
        model: &ModelChoice::default(),
        args: &[],
        session: None,
        fork: false,
        restore: None,
        parent: None,
    };
    assert_eq!(
        beside.relaunch(None),
        [
            "start",
            "--chat",
            "--mode",
            "plan",
            "--in",
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
    let repo = ChatRepository::Registered(repo);
    assert_eq!(topic.home(&repo).path(), Path::new("/work/worker"));
    let settings = crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap();
    let told = brief(&settings, &agent, &repo, &topic, false)
        .unwrap()
        .system;
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
    let at_commit = Topic::workspace(workspace, CHAT_MODE);
    let told = brief(&settings, &agent, &repo, &at_commit, false)
        .unwrap()
        .system;
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
    assert!(ensure_hosts_chat(&workspace).is_err());
    workspace.state = crate::state::WorkspaceState::Ready;
    workspace.path = root.path().join("gone");
    assert!(ensure_hosts_chat(&workspace).is_err());
}

#[test]
fn a_restored_chat_takes_the_issue_its_saved_chat_is_linked_to() {
    let repo = Repository {
        id: "repo".into(),
        path: "/repos/shoal".into(),
        source: "/repos/shoal".into(),
        last_used: 0,
        name: Some("shoal".into()),
        workspaces_dir: None,
        contribution: false,
    };
    let url = "https://forge.example/team/repo/issues/7";
    let chat: Chat = serde_json::from_value(serde_json::json!({
        "id": "c", "number": 4, "repository_id": "repo", "name": "shoal-plan-7",
        "agent": "claude", "mode": "plan", "pid": 1, "started_at": 0,
        "links": {"issue": url, "issue_title": "Decide", "prs": []},
    }))
    .unwrap();
    let chats = [chat];
    let restored = with_saved_issue(Topic::repository(&repo, "plan"), &chats, "c");
    let issue = restored.linked_issue.unwrap();
    assert_eq!((issue.number, issue.url.as_str()), (7, url));
    let other = with_saved_issue(Topic::repository(&repo, "plan"), &chats, "d");
    assert!(other.linked_issue.is_none());
}

#[test]
fn an_imported_chat_reopens_with_its_issue_without_asking_the_forge() {
    use clap::Parser;
    let repo = Repository {
        id: "repo".into(),
        path: "/repos/shoal".into(),
        source: "/repos/shoal".into(),
        last_used: 0,
        name: Some("shoal".into()),
        workspaces_dir: None,
        contribution: false,
    };
    let url = "https://forge.example/team/repo/issues/7";
    let links: SpaceLinks = serde_json::from_value(serde_json::json!({
        "issue": url, "issue_title": "Decide", "prs": [],
    }))
    .unwrap();
    let agent: Agent = "claude".parse().unwrap();
    let topic = Topic::imported(&repo, "plan", &links);
    let beside = Beside {
        topic: &topic,
        name: "shoal-plan-7",
        agent: &agent,
        model: &ModelChoice::default(),
        args: &[],
        session: Some("s1"),
        fork: false,
        restore: None,
        parent: None,
    };
    // The process the import starts in a new tab takes the issue as given.
    let relaunch = beside.relaunch(None);
    let cli =
        crate::cli::Cli::try_parse_from(std::iter::once("shoal".into()).chain(relaunch)).unwrap();
    let Some(crate::cli::Command::Create(crate::cli::args::create::CreateCommand::Start(start))) =
        cli.command
    else {
        panic!("expected start");
    };
    assert_eq!(start.issue, None);
    let issue =
        templates::IssueFields::at(&start.linked_issue.unwrap(), start.linked_issue_title).unwrap();
    assert_eq!((issue.number, issue.title.as_str()), (7, "Decide"));
    assert_eq!(start.session.as_deref(), Some("s1"));
    assert!(!start.fork_session);
    // A copy's chat starts from a fork of the session there too.
    let fork = Beside {
        fork: true,
        ..beside
    };
    let cli =
        crate::cli::Cli::try_parse_from(std::iter::once("shoal".into()).chain(fork.relaunch(None)))
            .unwrap();
    let Some(crate::cli::Command::Create(crate::cli::args::create::CreateCommand::Start(start))) =
        cli.command
    else {
        panic!("expected start");
    };
    assert_eq!(start.session.as_deref(), Some("s1"));
    assert!(start.fork_session);
    let unlinked = Topic::imported(&repo, "plan", &SpaceLinks::default());
    assert_eq!(unlinked.relaunch, Topic::repository(&repo, "plan").relaunch);
}

#[test]
fn a_chat_whose_agent_cannot_fork_starts_a_new_session() {
    let settings = crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap();
    let forked = SessionArgs {
        session: Some("s1"),
        fork: true,
        directory: Path::new("/repos/shoal"),
    };
    let claude: Agent = "claude".parse().unwrap();
    assert!(!forked.of(&settings, &claude).unwrap().is_empty());
    let opencode: Agent = "opencode".parse().unwrap();
    assert!(forked.of(&settings, &opencode).unwrap().is_empty());
}
