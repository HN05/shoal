use std::path::Path;

use super::*;
use crate::model::{Repository, Workspace};

fn repository() -> Repository {
    Repository {
        id: "repo".into(),
        path: "/repos/shoal".into(),
        source: "/repos/shoal".into(),
        last_used: 0,
        name: Some("shoal".into()),
        workspaces_dir: None,
        contribution: false,
    }
}

#[test]
fn a_chat_in_a_workspace_is_told_not_to_edit() {
    let mut workspace = Workspace::new_record(
        "repo".into(),
        "worker".into(),
        "/work/worker".into(),
        Some("feature".into()),
        crate::state::WorkspaceState::Ready,
    );
    let topic = Topic::workspace(workspace.clone(), &"plan".into());
    let agent: Agent = "claude".parse().unwrap();
    let repo = repository();
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
    let at_commit = Topic::workspace(workspace, &crate::config::modes::ModeName::CHAT);
    let told = brief(&settings, &agent, &repo, &at_commit, false)
        .unwrap()
        .system;
    assert!(told.contains("at commit 0123456"), "{told}");
}

#[test]
fn a_chat_whose_agent_cannot_fork_starts_a_new_session() {
    let settings = crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap();
    let forked = SessionArgs {
        session: Some(&"s1".into()),
        fork: true,
        directory: Path::new("/repos/shoal"),
    };
    let claude: Agent = "claude".parse().unwrap();
    assert!(!forked.of(&settings, &claude).unwrap().is_empty());
    let opencode: Agent = "opencode".parse().unwrap();
    assert!(forked.of(&settings, &opencode).unwrap().is_empty());
}

#[test]
fn a_converted_chat_is_told_what_its_space_is_while_it_continues_its_session() {
    let settings = crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap();
    let agent: Agent = "claude".parse().unwrap();
    let repo = repository();
    let topic = Topic::repository(&repo, &crate::config::modes::ModeName::CHAT);
    let converted =
        Topic::repository(&repo, &crate::config::modes::ModeName::CHAT).converted(&repo.path);
    let repo = ChatRepository::Registered(repo);

    let resumed = brief(&settings, &agent, &repo, &topic, true).unwrap();
    let told = brief(&settings, &agent, &repo, &converted, true).unwrap();

    assert_eq!(resumed.first_message, None);
    let message = told
        .first_message
        .expect("a converted chat's first message");
    assert!(message.contains("/repos/shoal"), "{message}");
}
