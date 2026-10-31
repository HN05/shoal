use std::path::Path;

use super::{commit, local};

#[test]
fn committed_setup_keeps_its_branch_and_instructions() {
    assert_eq!(commit::BRANCH, "shoal-setup");
    assert_eq!(
        commit::prompt("app", "shoal-setup"),
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
fn local_setup_saves_the_local_config_and_its_scripts_without_committing() {
    let prompt = local::prompt("app", Path::new("/shoal/app/.shoal/config.toml"));
    for expected in [
        "this chat runs in its registered checkout",
        "create or update its local config, `/shoal/app/.shoal/config.toml`,",
        "Save the scripts the config runs in `/shoal/app/.shoal` beside it",
        "paths relative to that directory",
        "commit nothing",
        "`shoal add app shoal-setup-trial`",
        "`shoal rm <workspace> -y`",
        "tell the user to check the config with `shoal config show --repo app`.",
    ] {
        assert!(
            prompt.contains(expected),
            "{expected:?} missing from {prompt}"
        );
    }
    assert!(!prompt.contains("commit the config"));
}
