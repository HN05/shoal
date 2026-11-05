use std::path::Path;

use super::{commit, local};

#[test]
fn committed_setup_names_the_repository_and_its_branch() {
    let prompt = commit::prompt("app", "setup-branch");
    for expected in ["the app repository", "branch setup-branch."] {
        assert!(
            prompt.contains(expected),
            "{expected:?} missing from {prompt}"
        );
    }
}

#[test]
fn local_setup_names_the_local_config_and_its_directory() {
    let prompt = local::prompt("app", Path::new("/shoal/app/.shoal/config.toml"));
    for expected in [
        "`/shoal/app/.shoal/config.toml`",
        "`/shoal/app/.shoal`",
        "`shoal start app shoal-setup-trial`",
        "`shoal config show --repo app`",
    ] {
        assert!(
            prompt.contains(expected),
            "{expected:?} missing from {prompt}"
        );
    }
}
