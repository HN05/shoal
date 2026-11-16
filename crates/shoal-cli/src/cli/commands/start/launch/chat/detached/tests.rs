use shoal_protocol::launches::StartTarget;
use shoal_protocol::space_names::SpaceSelector;
use shoal_schedule::priority::Priority;

use super::*;
use crate::{
    cli::commands::start::{ChatPlace, Topic},
    config::{modes::ModeName, permissions::PermissionsConfig},
    model::Repository,
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

/// Options of a new chat that runs detached, with the task `args`.
fn options() -> Options {
    Options {
        now: false,
        agent: Some("claude".into()),
        place: ChatPlace::Detached,
        name: None,
        model: Default::default(),
        args: vec!["Read the build".into()],
        session: None,
        fork: false,
        handoff: None,
        parent: Some(SpaceSelector::from("7")),
        narrowing: Narrowing::Requested(PermissionsConfig::default()),
        priority: Some(Priority::URGENT),
    }
}

#[test]
fn a_new_chat_on_an_issue_becomes_the_daemons_start_in_its_repository() {
    let topic = Topic::repository(&repository(), &ModeName::CHAT);
    let issue = "https://forge.example/team/app/issues/4".parse().unwrap();
    let topic = Topic {
        relaunch: topic.relaunch.clone().on_issue(&issue),
        ..topic
    };
    let (agent, name) = (Agent::from("claude"), SpaceName::from("talk"));
    let launch = request(&topic, &options(), (&agent, &name)).unwrap();
    assert_eq!(launch.target, Some(StartTarget::Repository("repo".into())));
    assert_eq!(launch.in_workspace, None);
    assert!(launch.chat);
    assert_eq!(launch.issue, Some(IssueChoice::Named(issue.into())));
    assert_eq!(launch.mode, Some(ModeName::CHAT));
    assert_eq!(launch.parent, Some(SpaceSelector::from("7")));
    assert_eq!(launch.task, ["Read the build"]);
    assert_eq!(launch.agent, Some("claude".into()));
    assert_eq!(launch.name, Some("talk".into()));
    assert_eq!(launch.priority, Some(Priority::URGENT));
}

#[test]
fn a_continued_session_starts_here() {
    let topic = Topic::repository(&repository(), &ModeName::CHAT);
    let session = Options {
        session: Some("s1".into()),
        ..options()
    };
    let recorded = Options {
        narrowing: Narrowing::Recorded(PermissionsConfig::default()),
        ..options()
    };
    let (agent, name) = (Agent::from("claude"), SpaceName::from("talk"));
    for options in [session, recorded] {
        assert!(request(&topic, &options, (&agent, &name)).is_none());
    }
}
