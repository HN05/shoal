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
fn the_flags_only_this_process_reads_fit_only_a_workspace() {
    for flag in ["--focus", "--fork"] {
        let args = start(&["repo", flag]);
        assert_eq!(fitting_kind(&args), Some(SpaceKind::Workspace), "{flag}");
        let error = ensure_fits(&args, SpaceKind::Chat, Some(&"plan".into())).unwrap_err();
        assert!(
            error.to_string().starts_with(&format!("{flag} ")),
            "{error}"
        );
    }
}

#[tokio::test]
async fn the_setup_mode_starts_only_with_its_own_commands() {
    let home = tempfile::tempdir().unwrap();
    let ctx = Context::new(crate::paths::Paths::for_test(home.path()), true);
    for args in [&["--mode", "setup"][..], &["--mode", "setup", "--as-chat"]] {
        let error = run(&ctx, start(args)).await.unwrap_err().to_string();
        assert!(error.contains("shoal config setup"), "{error}");
    }
}

#[test]
fn a_workspace_start_takes_the_checked_repository_by_id() {
    let checked = crate::repository::Selector::Id("repo-id".into());
    let launch = |target: &str, repository: Option<&str>| Launch {
        target: Some(target.parse().unwrap()),
        repository: repository.map(Into::into),
        ..Launch::default()
    };
    // It replaces the positional repository that named it.
    let named = launch::workspace::checked_launch(launch("repo", None), Some(&checked));
    assert_eq!(
        (named.target, named.repository),
        (None, Some("repo-id".into()))
    );
    // A positional issue stays, beside the repository `--repo` named.
    let issue = launch::workspace::checked_launch(launch("37", Some("repo")), Some(&checked));
    assert_eq!(issue.target, Some("37".parse().unwrap()));
    assert_eq!(issue.repository, Some("repo-id".into()));
    // Unchecked, `--repo` stays the user's text.
    let unchecked = launch::workspace::checked_launch(launch("37", Some("repo")), None);
    assert_eq!(unchecked.repository, Some("repo".into()));
}
