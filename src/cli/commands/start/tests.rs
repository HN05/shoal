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
