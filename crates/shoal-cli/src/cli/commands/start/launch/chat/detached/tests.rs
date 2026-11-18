use std::path::Path;

use shoal_protocol::launches::StartTarget;
use shoal_protocol::space_names::SpaceSelector;
use shoal_schedule::priority::Priority;

use super::*;
use crate::{
    cli::commands::start::{ChatPlace, Narrowing, Topic},
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
        workspaces_dir: "/tmp/shoal/repo".into(),
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
        chat: None,
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
fn the_config_setup_becomes_the_daemons_setup_of_the_global_config() {
    let topic = crate::cli::commands::config_setup::topic(Path::new("/home/me/config.toml"));
    let options = Options {
        parent: None,
        priority: None,
        ..options()
    };
    let (agent, name) = (Agent::from("claude"), SpaceName::from("setup"));
    let launch = request(&topic, &options, (&agent, &name)).unwrap();
    assert_eq!(launch.setup, Some(Setup::Config));
    assert!(!launch.chat);
    assert_eq!(launch.target, None);
    assert_eq!(launch.mode, Some(ModeName::SETUP));
    let prompt = launch.prompt.unwrap();
    assert!(prompt.contains("`/home/me/config.toml`"), "{prompt}");
    assert_eq!(launch.task, ["Read the build"]);
}

#[cfg(unix)]
#[test]
fn an_argument_that_is_not_utf8_is_refused_by_name() {
    use std::os::unix::ffi::OsStringExt;
    let topic = Topic::repository(&repository(), &ModeName::CHAT);
    let options = Options {
        args: vec![std::ffi::OsString::from_vec(vec![b'a', 0xff])],
        ..options()
    };
    let (agent, name) = (Agent::from("claude"), SpaceName::from("talk"));
    let error = request(&topic, &options, (&agent, &name)).unwrap_err();
    assert!(error.to_string().contains(r#""a\xFF""#), "{error:#}");
}
