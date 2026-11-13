use crate::client::{priority::Priority, space_names::SpaceSelector};

use super::*;
use crate::{
    cli::commands::chat::{ChatPlace, Topic},
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
    let start = request(&topic, &options(), (&agent, &name)).unwrap();
    assert_eq!(
        start.repository.as_ref().map(|named| named.value()),
        Some("repo")
    );
    assert_eq!(start.workspace, None);
    assert_eq!(start.issue, Some(issue.into()));
    assert_eq!(start.mode, Some(ModeName::CHAT));
    assert_eq!(start.parent, Some(SpaceSelector::from("7")));
    assert_eq!(start.args, ["Read the build"]);
    assert_eq!(start.agent, Some("claude".into()));
    assert_eq!(start.name, Some("talk".into()));
    assert_eq!(start.priority, Some(Priority::URGENT));
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
