use clap::Parser;

use super::ensure_flags_fit;
use crate::{
    cli::{Cli, Command, args::create::CreateCommand, args::create::StartArgs},
    model::SpaceKind,
};

fn start(args: &[&str]) -> StartArgs {
    let cli = Cli::try_parse_from(["shoal", "start"].iter().chain(args)).unwrap();
    let Some(Command::Create(CreateCommand::Start(start))) = cli.command else {
        panic!("expected start");
    };
    *start
}

#[test]
fn a_chat_mode_refuses_the_copy_and_fork_its_pickers_would_ask_for() {
    for flag in ["--copy", "--fork"] {
        let args = start(&["repo", "-m", "chat", flag]);
        let mode = "chat".into();
        let error = ensure_flags_fit(&args, SpaceKind::Chat, &mode).unwrap_err();
        assert!(
            error.to_string().starts_with(&format!("{flag} ")),
            "{error}"
        );
        ensure_flags_fit(&args, SpaceKind::Workspace, &mode).unwrap();
    }
}

#[test]
fn ci_takes_a_run_or_a_picker_and_names_the_work_alone() {
    let run = "https://forge.example/team/app/actions/runs/2765";
    let parse = |args: &[&str]| Cli::try_parse_from(["shoal", "start"].iter().chain(args));
    assert!(parse(&["--ci", "https://forge.example/team/app/issues/3"]).is_err());
    for flagged in [run, "2765"] {
        let flagged = start(&["--ci", flagged]).launch();
        super::check_start_arguments(&flagged).unwrap();
    }
    super::check_start_arguments(&start(&["--ci"]).launch()).unwrap();
    for refused in [
        &["--ci", run, "--branch", "fix"][..],
        &["--ci", "--issue", "7"],
        &["--ci", run, "https://forge.example/team/app/pulls/3"],
        &[run, "--pr", "3"],
    ] {
        let launch = start(refused).launch();
        assert!(
            super::check_start_arguments(&launch).is_err(),
            "{refused:?}"
        );
    }
}
