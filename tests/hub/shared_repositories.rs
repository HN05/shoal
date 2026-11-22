//! Shared repositories across the hub: sharing a repository on one machine
//! registers and clones it on every other, a machine that joins later
//! included, and unsharing it leaves their registrations, no longer
//! shared.
use std::path::Path;

use serde_json::Value;

use super::{
    directory::{give_origin, join, machine},
    support::{fixture::Base, wait::wait_for},
};

/// The registration of `machine` whose source is `origin`, once it is
/// shared as `shared` says.
fn registration(machine: &Base, origin: &str, shared: bool) -> Value {
    wait_for(&format!("{origin} registered, shared: {shared}"), || {
        let repositories = machine.ok(&["repo", "ls"]);
        (repositories.as_array().unwrap().iter())
            .find(|repository| {
                repository["source"] == origin && repository["shared_url"].is_string() == shared
            })
            .cloned()
    })
}

#[test]
fn a_shared_repository_is_cloned_on_every_machine_and_unsharing_it_leaves_their_clones() {
    let hub = machine(true, "app");
    let desk = machine(false, "app");
    join(&desk, &hub, "desk");
    let origin = give_origin(&desk);
    let repo = desk.repo.to_str().unwrap();
    let shared = desk.ok(&["repo", "share", repo]);
    assert_eq!(shared["shared_url"], origin.as_str());
    let cloned = registration(&hub, &origin, true);
    assert_eq!(cloned["shared_url"], origin.as_str());
    let path = cloned["path"].as_str().unwrap();
    assert!(Path::new(path).join(".git").exists(), "{cloned}");
    // A machine that joins later clones it too.
    let laptop = machine(false, "app");
    join(&laptop, &hub, "laptop");
    let laptops = registration(&laptop, &origin, true);
    desk.ok(&["repo", "unshare", repo]);
    for (machine, registered) in [(&hub, &cloned), (&laptop, &laptops)] {
        let kept = registration(machine, &origin, false);
        assert_eq!(kept["id"], registered["id"]);
    }
}
