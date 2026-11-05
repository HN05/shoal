use clap::Parser;

use super::*;
use crate::cli::{Cli, Command, args::create::CreateCommand};

fn start(args: &[&str]) -> StartArgs {
    let cli = Cli::try_parse_from(["shoal", "start"].iter().chain(args)).unwrap();
    let Some(Command::Create(CreateCommand::Start(start))) = cli.command else {
        panic!("expected start");
    };
    *start
}

#[test]
fn a_start_refuses_flags_its_kind_of_space_cannot_use() {
    let chat = |args: &[&str]| ensure_fits(&start(args), SpaceKind::Chat, "plan");
    chat(&["repo", "--issue", "7", "--name", "talk"]).unwrap();
    for (args, flag) in [
        (&["repo", "topic"][..], "<branch>"),
        (&["repo", "--base", "main"], "--base"),
        (&["repo", "--path", "/work/x"], "--path"),
        (&["repo", "--skip-setup"], "--skip-setup"),
        (&["repo", "--now"], "--now"),
    ] {
        // The refusal names the flag and the kind of space.
        let error = chat(args).unwrap_err().to_string();
        assert!(
            error.starts_with(&format!("{flag} ")) && error.contains("chat"),
            "{error}"
        );
    }
    let workspace = |args: &[&str]| ensure_fits(&start(args), SpaceKind::Workspace, "worker");
    workspace(&["repo", "topic", "--base", "main"]).unwrap();
    let error = workspace(&["repo", "--name", "talk"])
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("--name ") && error.contains("workspace"),
        "{error}"
    );
}

#[test]
fn a_chat_takes_a_positional_issue_as_its_issue() {
    let target = |positional: &str, repository: Option<&str>, issue: Option<Option<&str>>| {
        chat_target(
            Some(positional.to_owned()),
            repository.map(str::to_owned),
            issue.map(|issue| issue.map(str::to_owned)),
            false,
        )
    };
    let url = "https://forge.example/team/repo/issues/37";
    assert_eq!(
        target("37", Some("repo"), None).unwrap(),
        (Some("repo".into()), Some(Some("37".into())))
    );
    assert_eq!(
        target(url, None, None).unwrap(),
        (None, Some(Some(url.into())))
    );
    assert_eq!(
        target("repo", None, Some(None)).unwrap(),
        (Some("repo".into()), Some(None))
    );
    for (positional, repository, issue, refusal) in [
        ("37", None, Some(Some("38")), "name the issue once"),
        (
            "repo",
            Some("other"),
            None,
            "cannot be combined with --repo",
        ),
        (
            "https://forge.example/team/repo/pulls/4",
            None,
            None,
            "a chat starts on an issue",
        ),
    ] {
        let error = target(positional, repository, issue)
            .unwrap_err()
            .to_string();
        assert!(error.contains(refusal), "{positional}: {error}");
    }
}

#[test]
fn a_chat_in_a_workspace_takes_no_repository() {
    let in_workspace = |positional: Option<&str>, repository: Option<&str>| {
        chat_target(
            positional.map(str::to_owned),
            repository.map(str::to_owned),
            None,
            true,
        )
    };
    assert_eq!(
        in_workspace(Some("37"), None).unwrap(),
        (None, Some(Some("37".into())))
    );
    for (positional, repository) in [(None, Some("other")), (Some("other"), None)] {
        let error = in_workspace(positional, repository)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "a chat with --in starts in the workspace's repository; drop the repository"
        );
    }
}
