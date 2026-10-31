use std::path::PathBuf;

use super::{Destination, prompt};

#[test]
fn committed_setup_keeps_its_branch_and_instructions() {
    assert_eq!(Destination::Commit.branch(), "shoal-setup");
    assert_eq!(
        prompt("app", "shoal-setup", &Destination::Commit),
        "Set up Shoal for the app repository; this workspace is on branch shoal-setup. \
         Shoal gives each task its own worktree, and the repository config prepares \
         those worktrees and coordinates what tasks share. Study how the project is \
         built, tested and run, then create or update its config in `.shoal.toml`, or \
         in `.shoal/config.toml` when that file already exists, with only what the \
         project needs: a `setup.command` script that prepares a new worktree, named ports \
         for servers that several workspaces run at once, `[commands]` for frequent \
         tasks, and resource pools for services or devices that tasks must not use at \
         the same time. `shoal config reference --text` prints the reference that \
         describes the options.\n\n\
         Check the result with `shoal config show` and test the setup script with \
         `shoal work setup`, then commit the config and any scripts it runs."
    );
}

#[test]
fn local_setup_writes_the_local_config_and_its_scripts_without_committing() {
    let destination = Destination::Local(PathBuf::from("/shoal/app/.shoal/config.toml"));
    assert_eq!(destination.branch(), "shoal-setup-local");
    let prompt = prompt("app", "shoal-setup-local", &destination);
    for expected in [
        "this workspace is on branch shoal-setup-local",
        "create or update its local config, `/shoal/app/.shoal/config.toml`,",
        "Save the scripts the config runs in `/shoal/app/.shoal` beside it",
        "paths relative to that directory",
        "commit nothing",
        "tell the user to check it with `shoal config show --repo app`.",
    ] {
        assert!(
            prompt.contains(expected),
            "{expected:?} missing from {prompt}"
        );
    }
    assert!(!prompt.contains("commit the config"));
}
