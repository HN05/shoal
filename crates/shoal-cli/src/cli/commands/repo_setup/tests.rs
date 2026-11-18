use std::path::Path;

use super::{launch_options, subject};

#[test]
fn the_subject_names_the_repository_its_branch_and_its_local_config() {
    let subject = subject("app", Path::new("/shoal/app/.shoal/config.toml"));
    for expected in [
        "the app repository",
        "branch shoal-setup.",
        "`/shoal/app/.shoal/config.toml`",
        "`/shoal/app/.shoal`",
    ] {
        assert!(
            subject.contains(expected),
            "{expected:?} missing from {subject}"
        );
    }
}

#[test]
fn setup_focuses_its_agent_only_from_a_terminal_it_runs_in() {
    for (on_terminal, leaves, focused) in [
        (true, false, true),
        (false, true, false),
        // A scoped caller's start leaves it, even from a terminal.
        (true, true, false),
    ] {
        let options = launch_options(false, on_terminal, leaves);
        assert_eq!(options.focus, focused, "{on_terminal} {leaves}");
    }
}
