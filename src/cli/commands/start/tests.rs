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
    let chat = |args: &[&str]| ensure_fits(&start(args), SpaceKind::Chat, Some("plan"));
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
    let workspace = |args: &[&str]| ensure_fits(&start(args), SpaceKind::Workspace, Some("worker"));
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
        assert!(error.contains("--in"), "{error}");
    }
}

#[test]
fn a_copy_refuses_a_pr_or_branch_link_but_takes_an_issue() {
    let fits = |args: &[&str]| ensure_copy_fits(&start(args)).is_ok();
    assert!(fits(&["repo", "--copy", "topic"]));
    assert!(fits(&["https://forge.example/o/r/issues/7", "--copy"]));
    assert!(!fits(&[
        "https://forge.example/o/r/pulls/8",
        "--copy",
        "topic"
    ]));
    assert!(!fits(&[
        "https://forge.example/o/r/src/branch/feature",
        "--checkpoint",
        "2"
    ]));
    assert!(fits(&["https://forge.example/o/r/pulls/8"]));
}

#[test]
fn the_arguments_other_than_the_mode_decide_the_kind_they_fit() {
    let fits = |args: &[&str]| fitting_kind(&start(args));
    assert_eq!(fits(&["repo"]), None);
    assert_eq!(fits(&["https://forge.example/o/r/issues/7"]), None);
    assert_eq!(fits(&["repo", "topic"]), Some(SpaceKind::Workspace));
    assert_eq!(
        fits(&["repo", "--base", "main"]),
        Some(SpaceKind::Workspace)
    );
    assert_eq!(
        fits(&["https://forge.example/o/r/pulls/8"]),
        Some(SpaceKind::Workspace)
    );
    assert_eq!(fits(&["repo", "--name", "talk"]), Some(SpaceKind::Chat));
}

#[test]
fn an_issues_labels_name_its_mode() {
    let config = config::Issue {
        required_labels: Vec::new(),
        plan_label: Some("needs-plan".into()),
        planned_label: Some("planned".into()),
    };
    let mode = |labels: &[&str]| {
        let labels: Vec<String> = labels.iter().map(|label| (*label).into()).collect();
        labelled_mode(&config, &labels)
    };
    assert_eq!(mode(&["type/bug", "planned"]), Some(modes::DEFAULT));
    assert_eq!(mode(&["needs-plan"]), Some(modes::PLAN));
    // A settled plan wins over a stale label asking for one.
    assert_eq!(mode(&["needs-plan", "planned"]), Some(modes::DEFAULT));
    assert_eq!(mode(&["type/bug"]), None);
    let unset = config::Issue {
        plan_label: None,
        planned_label: None,
        ..config
    };
    assert_eq!(labelled_mode(&unset, &["needs-plan".into()]), None);
}

#[test]
fn a_start_names_an_issue_with_issue_or_positionally() {
    let issue = |args: &[&str]| named_issue(&start(args)).map(str::to_owned);
    let url = "https://forge.example/o/r/issues/7";
    assert_eq!(issue(&["repo", "--issue", "7"]).as_deref(), Some("7"));
    assert_eq!(issue(&["7"]).as_deref(), Some("7"));
    assert_eq!(issue(&[url]).as_deref(), Some(url));
    assert_eq!(issue(&["repo"]), None);
    assert_eq!(issue(&["https://forge.example/o/r/pulls/8"]), None);
    assert_eq!(issue(&["repo", "--issue"]), None);
}

#[tokio::test]
async fn the_setup_mode_starts_only_with_its_own_commands() {
    let home = tempfile::tempdir().unwrap();
    let ctx = Context::new(crate::paths::Paths::for_test(home.path()), true);
    for args in [&["--mode", "setup"][..], &["--mode", "setup", "--chat"]] {
        let error = run(&ctx, start(args)).await.unwrap_err().to_string();
        assert!(error.contains("shoal config setup"), "{error}");
    }
}
