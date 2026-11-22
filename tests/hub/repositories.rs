//! Repositories across the hub: each machine lists the other machines'
//! registrations after its own, and removes one on the machine that
//! registers it.
use serde_json::Value;

use super::{
    directory::{join, machine},
    support::{fixture::Base, wait::wait_for},
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
    assert_eq!(desk.ok(&["repo", "ls"]), serde_json::json!([]));
    assert!(!desk.repo.exists());
    // The hub's own repository stays.
    assert!(hub.repo.exists());
}
