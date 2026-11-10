use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use clap::Parser as _;
use std::ffi::OsString;

use super::super::{Narrowing, Relaunch};
use super::*;
use crate::agent::ModelChoice;
use crate::cli::args::create::{CreateCommand, StartArgs};
use crate::config::permissions::PermissionsConfig;
use crate::model::{Repository, SpaceLinks, Workspace};

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
        let beside = Beside {
            topic: &topic,
            name: &"named".into(),
            agent: &agent,
            options: &options,
        };
        let handoff = Path::new("/tmp/shoal-chat.1");
        let mut start = parsed_start(beside.relaunch(Some(handoff)));
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
fn a_chat_in_a_workspace_reopens_there_and_is_told_not_to_edit() {
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
    let beside = Beside {
        topic: &topic,
        name: &"worker-chat".into(),
        agent: &agent,
        options: &options,
    };
    let start = parsed_start(beside.relaunch(None));
    assert_eq!(
        start.in_workspace,
        Some(Some(SpaceSelector::parse("worker-id")))
    );
    assert_eq!((start.mode, start.repository), (Some("plan".into()), None));

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
    let beside = Beside {
        topic: &topic,
        name: &"shoal-plan-7".into(),
        agent: &agent,
        options: &options,
    };
    // The process the import starts in a new tab takes the issue as given.
    let start = parsed_start(beside.relaunch(None));
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
fn the_setup_chat_starts_again_with_config_setup_and_its_options() {
    let topic = crate::cli::commands::config_setup::topic(Path::new("/config/config.toml"));
    let agent: Agent = "claude".parse().unwrap();
    let options = Options {
        fork: false,
        parent: None,
        narrowing: Narrowing::Requested(Default::default()),
        ..options(Narrowing::Requested(Default::default()))
    };
    let beside = Beside {
        topic: &topic,
        name: &"setup".into(),
        agent: &agent,
        options: &options,
    };
    let line = std::iter::once(OsString::from("shoal")).chain(beside.relaunch(None));
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
