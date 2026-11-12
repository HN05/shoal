use std::path::Path;

use super::{Options, add_options, subject};

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
fn setup_focuses_its_agent_only_from_a_terminal() {
    for on_terminal in [true, false] {
        let options = Options {
            here: false,
            agent: None,
            model: Default::default(),
            args: Vec::new(),
        };
        let options = add_options(&options, "Set up".into(), on_terminal);
        assert!(options.now);
        assert_eq!(options.focus, on_terminal);
    }
}
