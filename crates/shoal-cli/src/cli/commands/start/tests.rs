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
    let checked = || Some(crate::repository::Selector::Id("repo-id".into()));
    let id = |named: Option<crate::repository::Selector>| match named {
        Some(crate::repository::Selector::Id(id)) => Some(id),
        _ => None,
    };
    // It replaces the positional repository that named it.
    let (positional, named) =
        named_repository(Some("repo".parse().unwrap()), None, checked()).unwrap();
    assert_eq!((positional, id(named)), (None, Some("repo-id".into())));
    // A positional issue stays, beside the repository `--repo` named.
    let (positional, named) =
        named_repository(Some("37".parse().unwrap()), Some("repo".into()), checked()).unwrap();
    assert_eq!(
        (positional, id(named)),
        (Some("37".parse().unwrap()), Some("repo-id".into()))
    );
    // Unchecked, `--repo` stays the user's text.
    let (_, named) =
        named_repository(Some("37".parse().unwrap()), Some("repo".into()), None).unwrap();
    assert!(matches!(named, Some(crate::repository::Selector::Value(value)) if value == "repo"));
}
