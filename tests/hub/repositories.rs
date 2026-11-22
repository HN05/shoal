//! Repositories across the hub: each machine lists the other machines'
//! registrations after its own, and removes one on the machine that
//! registers it.
use serde_json::Value;

use super::{
    directory::{join, machine, workspace},
    support::{fixture::Base, shoal_binary, wait::wait_for},
};

/// The registration `machine` lists for the machine `on`, once it does.
fn listed_on(machine: &Base, on: &str) -> Value {
    wait_for(&format!("a repository of {on} listed"), || {
        let repositories = machine.ok(&["repo", "ls"]);
        (repositories.as_array().unwrap().iter())
            .find(|repository| repository["machine"] == on)
            .cloned()
    })
}

#[test]
fn a_machine_lists_the_other_machines_repositories_after_its_own() {
    let hub = machine(true, "app");
    let desk = machine(false, "web");
    join(&desk, &hub, "desk");
    let desks = desk.ok(&["repo", "ls"])[0].clone();
    listed_on(&hub, "desk");
    let listed = hub.ok(&["repo", "ls"]);
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert!(listed[0]["machine"].is_null(), "{listed:?}");
    assert_eq!(listed[1]["id"], desks["id"]);
    assert_eq!(listed[1]["name"], "web");
}

#[test]
fn a_repository_registered_only_on_another_machine_is_removed_there() {
    let hub = machine(true, "app");
    let desk = machine(false, "web");
    join(&desk, &hub, "desk");
    let desks = desk.ok(&["repo", "ls"])[0].clone();
    listed_on(&hub, "desk");
    let removed = hub.ok(&["repo", "rm", "web", "--yes"]);
    assert_eq!(removed[0]["machine"], "desk");
    assert_eq!(removed[0]["repository_id"], desks["id"]);
    // Desk still lists the hub's repository, which is not its own.
    let left = desk.ok(&["repo", "ls"]);
    let left = left.as_array().unwrap();
    assert!(
        left.iter().all(|listed| listed["machine"].is_string()),
        "{left:?}"
    );
    assert!(!desk.repo.exists());
    // The hub's own repository stays.
    assert!(hub.repo.exists());
}

#[test]
fn a_space_lists_no_other_machine_s_repository_of_another_origin() {
    let hub = machine(true, "app");
    let desk = machine(false, "web");
    join(&desk, &hub, "desk");
    listed_on(&hub, "desk");
    let mine = workspace(&hub, "mine");
    let mut command = hub.command();
    command.args(["run", "--workspace", &mine, "--", shoal_binary()]);
    let listed = hub.ok_from(&mut command, &["repo", "ls"]);
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert!(listed[0]["machine"].is_null(), "{listed:?}");
}
