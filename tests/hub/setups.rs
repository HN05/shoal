//! Setting up a workspace another joined machine runs, from the hub: its
//! setup and post-setup hook run there, in that machine daemon's
//! environment.
use std::{fs, path::PathBuf};

use serde_json::Value;

use super::{
    directory::{join, listed, machine},
    support::{
        fixture::{Base, home},
        git, git_with_identity, install_test_script, printed,
    },
};

/// Setup records the `SETUP_MARK` of the environment it runs in, and fails
/// while the worktree holds `fail-setup`.
const SETUP: &str = "#!/bin/sh\nprintf '%s' \"$SETUP_MARK\" > .setup-ran\ntest ! -e fail-setup\n";

/// The post-setup hook records the `SETUP_MARK` of its environment.
const POST_SETUP: &str = "#!/bin/sh\nprintf '%s' \"$SETUP_MARK\" > .post-setup-ran\n";

/// The hub, and a laptop joined to it whose daemon runs with
/// `SETUP_MARK=laptop` and whose repository sets up workspaces with the
/// scripts above, with the laptop's ID at the hub.
fn hub_and_laptop() -> (Base, Base, String) {
    let env = [("SETUP_MARK", "laptop")];
    let laptop = Base::start_with_env(home(), ("", &env), "app", &[]);
    let repo = &laptop.repo;
    install_test_script(repo.join("setup.sh"), SETUP);
    install_test_script(repo.join("post-setup.sh"), POST_SETUP);
    let ignored = ".setup-ran\n.post-setup-ran\nfail-setup\n";
    fs::write(repo.join(".gitignore"), ignored).unwrap();
    let config = "setup.command = 'setup.sh'\nhooks.post_setup = 'post-setup.sh'\n";
    fs::write(repo.join(".shoal.toml"), config).unwrap();
    git(repo, &["add", "."]);
    git_with_identity(repo, &["commit", "-q", "-m", "setup"]);
    let hub = machine(true, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    (hub, laptop, laptop_id)
}

/// The laptop's new workspace on `branch`, made from `args` besides it,
/// once the hub lists it, by ID, with its worktree.
fn laptop_workspace(
    (hub, laptop, laptop_id): (&Base, &Base, &str),
    args: &[&str],
) -> (String, PathBuf) {
    let created = laptop.ok(&[&["create"], args].concat());
    let id = created["id"].as_str().unwrap().to_owned();
    listed(hub, &id, laptop_id);
    let path = created["path"].as_str().unwrap().into();
    (id, path)
}

/// What the hub's user prints for `args`, which must succeed, with
/// `SETUP_MARK=hub` in the CLI's environment.
fn on_hub(hub: &Base, args: &[&str]) -> Value {
    hub.ok_from(hub.command().env("SETUP_MARK", "hub"), args)
}

fn read(path: PathBuf) -> String {
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn setup_of_another_machine_s_workspace_runs_there_in_its_daemon_s_environment() {
    let (hub, laptop, laptop_id) = hub_and_laptop();
    let repo = laptop.repo.to_str().unwrap().to_owned();
    let machines = (&hub, &laptop, laptop_id.as_str());
    let (there, path) = laptop_workspace(machines, &[&repo, "--branch", "on-laptop"]);
    let set_up = on_hub(&hub, &["setup", &there]);
    assert_eq!(set_up["state"], "ready", "{set_up}");
    assert_eq!(read(path.join(".setup-ran")), "laptop");
    assert_eq!(read(path.join(".post-setup-ran")), "laptop");
}

#[test]
fn a_failed_setup_elsewhere_names_its_commands_and_skip_runs_the_hook_there() {
    let (hub, laptop, laptop_id) = hub_and_laptop();
    let repo = laptop.repo.to_str().unwrap().to_owned();
    let machines = (&hub, &laptop, laptop_id.as_str());
    let (there, path) = laptop_workspace(machines, &[&repo, "--branch", "on-laptop"]);
    fs::remove_file(path.join(".post-setup-ran")).unwrap();
    fs::write(path.join("fail-setup"), "").unwrap();
    let failed = hub.run(&["--json", "setup", &there]);
    assert!(!failed.status.success(), "{}", printed(&failed));
    let number = listed(&hub, &there, &laptop_id)["number"].to_string();
    let error = String::from_utf8_lossy(&failed.stderr);
    assert!(
        error.contains(&format!("shoal setup {number} --skip"))
            && error.contains(&format!("shoal rm {number} --yes")),
        "{}",
        printed(&failed)
    );
    assert_eq!(read(path.join(".setup-ran")), "laptop");
    assert_eq!(
        laptop.ok(&["status", &there])["workspace"]["state"],
        "failed"
    );
    let skipped = on_hub(&hub, &["setup", &there, "--skip"]);
    assert_eq!(skipped["state"], "ready", "{skipped}");
    assert_eq!(read(path.join(".post-setup-ran")), "laptop");
}

#[test]
fn a_refresh_of_another_machine_s_copy_reruns_setup_there() {
    let (hub, laptop, laptop_id) = hub_and_laptop();
    let repo = laptop.repo.to_str().unwrap().to_owned();
    let machines = (&hub, &laptop, laptop_id.as_str());
    let (source, source_path) = laptop_workspace(machines, &[&repo, "--branch", "source"]);
    let (copy, path) = laptop_workspace(machines, &["--copy", &source]);
    // Its source's files change, which the copy takes again over what its
    // setup wrote.
    fs::write(source_path.join("notes"), "new in the source\n").unwrap();
    let refreshed = on_hub(&hub, &["refresh", &copy]);
    assert_eq!(refreshed["up_to_date"], false, "{refreshed}");
    assert_eq!(read(path.join("notes")), "new in the source\n");
    assert_eq!(read(path.join(".setup-ran")), "laptop");
}
