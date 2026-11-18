//! Requests for a space another joined machine owns, which the caller's
//! daemon forwards there through the hub, and the events a space passes up
//! to a coordinator on another machine.
use super::{
    directory::{join, listed, machine, workspace},
    support::{fixture::Base, printed},
};

/// The hub and a laptop joined to it, with a workspace on the laptop that
/// the hub lists, by ID.
fn laptop_space() -> (Base, Base, String) {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    let there = workspace(&laptop, "on-laptop");
    listed(&hub, &there, &laptop_id);
    (hub, laptop, there)
}

#[test]
fn the_user_s_terminal_renames_and_removes_a_space_on_another_machine() {
    let (hub, laptop, there) = laptop_space();
    hub.ok(&["rename", &there, "Renamed there"]);
    let status = laptop.ok(&["status", &there]);
    assert_eq!(status["workspace"]["name"], "Renamed there", "{status}");
    hub.ok(&["rm", &there, "--wait", "--yes"]);
    let gone = laptop.run(&["status", &there, "--json"]);
    assert!(!gone.status.success(), "{}", printed(&gone));
}

#[test]
fn a_command_that_needs_the_space_s_files_names_its_machine() {
    let (hub, _laptop, there) = laptop_space();
    let output = hub.run(&["diff", &there]);
    assert!(!output.status.success(), "{}", printed(&output));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("laptop"), "{error}");
}
