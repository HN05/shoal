use clap::Parser as _;

use super::*;
use crate::config::permissions::{PermissionChange, PermissionsConfig};

/// The `shoal start` arguments `start`'s command line parses to.
fn reparsed(start: &StartArgs) -> StartArgs {
    let line = std::iter::once(OsString::from("shoal")).chain(start.command_line());
    let cli = crate::cli::Cli::try_parse_from(line).unwrap();
    let Some(crate::cli::Command::Create(CreateCommand::Start(parsed))) = cli.command else {
        panic!("expected start");
    };
    *parsed
}

fn assert_round_trips(start: StartArgs) {
    assert_eq!(format!("{:?}", reparsed(&start)), format!("{start:?}"));
}

#[test]
fn a_chats_start_parses_back_from_its_command_line() {
    let changes = ["push=propose", "merge=mode"]
        .map(|assignment| PermissionChange::assigned(assignment).unwrap());
    assert_round_trips(StartArgs {
        mode: Some("plan".into()),
        permissions: changes.to_vec(),
        chat: true,
        // Without a value, before another flag.
        in_workspace: Some(None),
        parent: Some(SpaceSelector::parse("7")),
        here: true,
        issue: Some(Some("12".into())),
        agent: Some("codex".parse().unwrap()),
        name: Some("named".into()),
        model: ModelArgs {
            model: Some("gpt".into()),
            effort: Some("high".into()),
        },
        session: Some("s1".into()),
        fork_session: true,
        restore: Some(SpaceId::from("c1")),
        linked_issue: Some("https://forge.example/team/repo/issues/7".into()),
        linked_issue_title: Some("Decide".into()),
        handoff: Some("/tmp/shoal-chat.1".into()),
        args: ["--search", "-x"].map(OsString::from).to_vec(),
        ..Default::default()
    });
    let narrowing = PermissionsConfig::assigned(&["push=deny".to_owned()]).unwrap();
    assert_round_trips(StartArgs {
        recorded_narrowing: Some(narrowing),
        chat: true,
        in_workspace: Some(Some(SpaceSelector::parse("worker"))),
        // Without a value, last before --.
        issue: Some(None),
        args: vec!["task".into()],
        ..Default::default()
    });
}

#[test]
fn a_workspaces_start_parses_back_from_its_command_line() {
    assert_round_trips(StartArgs {
        mode: Some("worker".into()),
        worktree: true,
        repository: Some("shoal".into()),
        repository_override: Some("team/shoal".into()),
        branch: Some("feature".into()),
        issue: Some(None),
        path: Some("/work/feature".into()),
        base: Some("origin/main".into()),
        git_profile: Some("work".into()),
        agent: Some("claude".parse().unwrap()),
        fork: true,
        after: vec!["12".into(), "other".into()],
        creation: CreationArgs {
            skip_setup: true,
            disposable: true,
        },
        ..Default::default()
    });
    assert_round_trips(StartArgs {
        repository: Some("shoal".into()),
        focus: true,
        copy: CopyArgs {
            copy: Some(None),
            checkpoint: Some(CheckpointNumber::from(3)),
        },
        ..Default::default()
    });
    assert_round_trips(StartArgs {
        repository: Some("shoal".into()),
        existing: Some("origin/feature".into()),
        now: true,
        ..Default::default()
    });
}
