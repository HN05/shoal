use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use clap::Parser as _;
use std::{ffi::OsString, path::Path};

use super::*;
use crate::{
    agent::{Agent, ModelChoice},
    cli::{
        args::create::{CreateCommand, StartArgs},
        commands::chat::{ChatPlace, Narrowing},
    },
    config::permissions::PermissionsConfig,
};

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

/// The `shoal start` arguments `line` parses to.
fn parsed_start(line: Vec<OsString>) -> StartArgs {
    let line = std::iter::once(OsString::from("shoal")).chain(line);
    let cli = crate::cli::Cli::try_parse_from(line).unwrap();
    let Some(crate::cli::Command::Create(CreateCommand::Start(start))) = cli.command else {
        panic!("expected start");
    };
    *start
}

/// Options a start beside this terminal was given.
fn options(narrowing: Narrowing) -> Options {
    Options {
        agent: None,
        place: ChatPlace::Beside,
        name: None,
        model: ModelChoice {
            model: None,
            effort: Some("high".into()),
        },
        args: vec!["--search".into()],
        session: Some("s1".into()),
        fork: true,
        restore: Some(SpaceId::from("c1")),
        handoff: None,
        parent: Some(SpaceSelector::parse("7")),
        narrowing,
    }
}

#[test]
fn a_chat_started_elsewhere_parses_back_to_its_options_with_its_name_and_agent() {
    let repo = repository();
    let topic = Topic::repository(&repo, &crate::config::modes::ModeName::CHAT);
    let agent: Agent = "codex".parse().unwrap();
    let requested = PermissionsConfig::assigned(&["push=propose".to_owned()]).unwrap();
    for narrowing in [
        Narrowing::Requested(requested),
        Narrowing::Recorded(requested),
    ] {
        let options = options(narrowing);
        let handoff = Path::new("/tmp/shoal-chat.1");
        let named = options.in_new_place(&agent, &"named".into(), Some(handoff));
        let mut start = parsed_start(topic.relaunch.command_line(named));
        assert!(start.chat);
        assert_eq!(start.mode, Some(crate::config::modes::ModeName::CHAT));
        assert_eq!(start.repository.as_deref(), Some("repo"));
        assert_eq!(
            Options::taken_from(&mut start),
            Options {
                agent: Some(agent.clone()),
                place: ChatPlace::Here,
                name: Some("named".into()),
                handoff: Some(handoff.to_owned()),
                ..options
            }
        );
    }
}

#[test]
fn a_fork_without_a_session_starts_elsewhere_as_a_new_session() {
    let repo = repository();
    let topic = Topic::repository(&repo, &crate::config::modes::ModeName::CHAT);
    let agent: Agent = "codex".parse().unwrap();
    let options = Options {
        session: None,
        ..options(Narrowing::Recorded(Default::default()))
    };
    let named = options.in_new_place(&agent, &"named".into(), None);
    let mut start = parsed_start(topic.relaunch.command_line(named));
    let started = Options::taken_from(&mut start);
    assert_eq!((started.session, started.fork), (None, false));
}

#[test]
fn a_chat_in_a_workspace_reopens_there() {
    let mut workspace = Workspace::new_record(
        "repo".into(),
        "worker".into(),
        "/work/worker".into(),
        Some("feature".into()),
        crate::state::WorkspaceState::Ready,
    );
    workspace.id = "worker-id".into();
    let topic = Topic::workspace(workspace.clone(), &"plan".into());
    assert_eq!(topic.name, "worker-chat");
    let agent: Agent = "claude".parse().unwrap();
    let options = Options {
        agent: None,
        place: ChatPlace::Beside,
        name: None,
        model: ModelChoice::default(),
        args: Vec::new(),
        session: None,
        fork: false,
        restore: None,
        handoff: None,
        parent: None,
        narrowing: Narrowing::Requested(Default::default()),
    };
    let named = options.in_new_place(&agent, &"worker-chat".into(), None);
    let start = parsed_start(topic.relaunch.command_line(named));
    assert_eq!(
        start.in_workspace,
        Some(Some(SpaceSelector::parse("worker-id")))
    );
    assert_eq!((start.mode, start.repository), (Some("plan".into()), None));
}

#[test]
fn an_imported_chat_reopens_with_its_issue_without_asking_the_forge() {
    let repo = repository();
    let url = "https://forge.example/team/repo/issues/7";
    let links: SpaceLinks = serde_json::from_value(serde_json::json!({
        "issue": url, "issue_title": "Decide", "prs": [],
    }))
    .unwrap();
    let agent: Agent = "claude".parse().unwrap();
    let topic = Topic::imported(&repo, &"plan".into(), &links);
    let options = Options {
        agent: None,
        place: ChatPlace::Beside,
        name: None,
        model: ModelChoice::default(),
        args: Vec::new(),
        session: Some("s1".into()),
        fork: false,
        restore: None,
        handoff: None,
        parent: None,
        narrowing: Narrowing::Recorded(Default::default()),
    };
    let named = options.in_new_place(&agent, &"shoal-plan-7".into(), None);
    // The process the import starts in a new tab takes the issue as given.
    let start = parsed_start(topic.relaunch.command_line(named));
    assert_eq!(start.issue, None);
    let issue =
        templates::IssueFields::at(&start.linked_issue.unwrap(), start.linked_issue_title).unwrap();
    assert_eq!((issue.number, issue.title.as_str()), (7, "Decide"));
    let Relaunch::Start(unlinked) =
        Topic::imported(&repo, &"plan".into(), &SpaceLinks::default()).relaunch
    else {
        panic!("expected a start");
    };
    assert_eq!(unlinked.linked_issue, None);
}

#[test]
fn the_setup_chat_starts_again_with_config_setup_and_its_options() {
    let topic = crate::cli::commands::config_setup::topic(Path::new("/config/config.toml"));
    let agent: Agent = "claude".parse().unwrap();
    let options = Options {
        fork: false,
        parent: None,
        narrowing: Narrowing::Requested(Default::default()),
        ..options(Narrowing::Requested(Default::default()))
    };
    let named = options.in_new_place(&agent, &"setup".into(), None);
    let line = std::iter::once(OsString::from("shoal")).chain(topic.relaunch.command_line(named));
    let cli = crate::cli::Cli::try_parse_from(line).unwrap();
    let Some(crate::cli::Command::Service(crate::cli::args::service::ServiceCommand::Config {
        command: crate::cli::args::service::ConfigCommand::Setup(setup),
    })) = cli.command
    else {
        panic!("expected config setup");
    };
    assert!(setup.here);
    assert_eq!(setup.agent, Some(agent));
    assert_eq!(setup.name, Some("setup".into()));
    assert_eq!(
        (setup.session, setup.restore),
        (options.session, options.restore)
    );
    assert_eq!(setup.args, options.args);
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
    let restored = with_saved(
        Topic::repository(&repo, &"plan".into()),
        &chats,
        &SpaceId::from("c"),
        &repo.path,
    );
    let issue = restored.linked_issue.unwrap();
    assert_eq!((issue.number, issue.url.as_str()), (7, url));
    let other = with_saved(
        Topic::repository(&repo, &"plan".into()),
        &chats,
        &SpaceId::from("d"),
        &repo.path,
    );
    assert!(other.linked_issue.is_none());
}

#[test]
fn a_restored_chat_that_was_a_workspace_is_told_so_in_the_place_it_opens() {
    let repo = Repository {
        id: "repo".into(),
        path: "/repos/shoal".into(),
        source: "/repos/shoal".into(),
        last_used: 0,
        name: Some("shoal".into()),
        workspaces_dir: None,
        contribution: false,
    };
    let saved = |converted: bool| -> Chat {
        serde_json::from_value(serde_json::json!({
            "id": "c", "number": 4, "repository_id": "repo", "name": "fix-login",
            "agent": "claude", "mode": "chat", "pid": 1, "started_at": 0,
            "restore": "resume", "converted": converted,
            "links": {"issue": "https://forge.example/team/repo/issues/7",
                "issue_title": "Fix the login", "prs": []},
        }))
        .unwrap()
    };
    // The topic a relaunch in a new place starts from names only the chat.
    let relaunched = || Topic::repository(&repo, &"chat".into());

    let converted = with_saved(
        relaunched(),
        &[saved(true)],
        &SpaceId::from("c"),
        &repo.path,
    );
    let plain = with_saved(
        relaunched(),
        &[saved(false)],
        &SpaceId::from("c"),
        &repo.path,
    );

    assert!(converted.tells_resumed);
    assert!(!plain.tells_resumed && plain.subject.is_none());
    // Its mode's instructions name the issue it was linked to as a workspace.
    let issue = converted.subject.and_then(|subject| subject.issue);
    assert_eq!(issue.map(|issue| issue.number), Some(7));
}
