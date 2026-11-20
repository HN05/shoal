use clap::CommandFactory;

use super::*;

/// Whether the command line `args` reaches other machines' spaces.
fn reaches(args: &[&str]) -> bool {
    let matches = crate::cli::Cli::command()
        .try_get_matches_from(args)
        .unwrap();
    let mut path = vec![];
    let mut leaf = &matches;
    while let Some((name, child)) = leaf.subcommand() {
        path.push(name);
        leaf = child;
    }
    reaches_elsewhere(&path, leaf)
}

#[test]
fn a_stop_of_this_machine_s_turns_or_processes_offers_only_its_spaces() {
    assert!(reaches(&["shoal", "stop"]));
    assert!(!reaches(&["shoal", "stop", "--interrupt"]));
    assert!(!reaches(&["shoal", "stop", "--processes"]));
    assert!(!reaches(&["shoal", "stop", "--leftovers"]));
}

#[test]
fn a_command_that_attaches_files_offers_only_this_machine_s_spaces() {
    assert!(reaches(&["shoal", "send", "Rebase onto main"]));
    assert!(!reaches(&[
        "shoal",
        "send",
        "See the log",
        "--attach",
        "log.txt"
    ]));
    assert!(reaches(&["shoal", "done"]));
    assert!(!reaches(&["shoal", "done", "--attach", "report.md"]));
}
