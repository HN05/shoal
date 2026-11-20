//! `shoal move --machine`: moving a space to another machine joined to the
//! same hub, whose claim of the space at the hub commits the move.
use std::fs;

use serde_json::{Value, json};

use super::directory::{give_origin, join, listed, machine, workspace};
use super::support::{
    fixture::{Base, home},
    git, git_with_identity, printed,
    wait::wait_for,
};

/// A hub with the joined machines `laptop` and `desk`, whose global config
/// is `desk_config`, with their IDs at the hub.
fn machines(desk_config: &str) -> (Base, Base, Base, String, String) {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    let desk = Base::start(home(), desk_config, "desk-app", &[]);
    let laptop_id = join(&laptop, &hub, "laptop");
    let desk_id = join(&desk, &hub, "desk");
    (hub, laptop, desk, laptop_id, desk_id)
}

/// The spaces `machine` runs itself.
fn own_spaces(machine: &Base) -> Vec<Value> {
    let listed = machine.ok(&["list"]);
    (listed.as_array().unwrap().iter())
        .filter(|space| space["machine"].is_null())
        .cloned()
        .collect()
}

#[test]
fn a_move_from_a_third_machine_takes_a_workspace_to_a_joined_one_with_its_id_and_number() {
    let (hub, laptop, desk, laptop_id, desk_id) = machines("");
    give_origin(&laptop);
    let work = workspace(&laptop, "work");
    let path = laptop.ok(&["status", &work])["workspace"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::write(format!("{path}/notes"), "only the laptop has this\n").unwrap();
    let path = std::path::Path::new(&path);
    git(path, &["add", "notes"]);
    git_with_identity(path, &["commit", "-q", "-m", "notes"]);
    let number = listed(&hub, &work, &laptop_id)["number"].clone();

    let moved = hub.ok(&["move", &work, "--machine", "desk"]);

    assert_eq!(moved["failed"], json!([]), "{moved}");
    assert_eq!(listed(&hub, &work, &desk_id)["number"], number);
    let there = desk.ok(&["status", &work]);
    let there = there["workspace"]["path"].as_str().unwrap();
    assert!(
        fs::read_to_string(format!("{there}/notes")).is_ok_and(|notes| notes.contains("laptop"))
    );
    assert!(own_spaces(&laptop).iter().all(|space| space["id"] != work));
}

#[test]
fn a_move_to_an_offline_machine_fails_at_once_naming_it() {
    let (_hub, laptop, mut desk, _laptop_id, _desk_id) = machines("");
    let work = workspace(&laptop, "work");
    desk.daemon.kill();

    let refused = laptop.run(&["move", &work, "--machine", "desk", "--json"]);

    assert!(!refused.status.success(), "{}", printed(&refused));
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("machine desk is offline"), "{error}");
    assert!(own_spaces(&laptop).iter().any(|space| space["id"] == work));
}

#[test]
fn a_space_the_destination_cannot_take_stays_on_its_machine() {
    let (_hub, laptop, desk, _laptop_id, _desk_id) = machines("");
    // Without an origin, the desk has nothing to clone the repository from.
    let work = workspace(&laptop, "work");

    let moved = laptop.run(&["move", &work, "--machine", "desk", "--json"]);

    assert!(!moved.status.success(), "{}", printed(&moved));
    let report: Value = serde_json::from_slice(&moved.stdout).unwrap();
    assert_eq!(report["failed"][0]["id"], work.as_str(), "{report}");
    assert!(own_spaces(&laptop).iter().any(|space| space["id"] == work));
    assert!(own_spaces(&desk).is_empty());
}

#[test]
fn a_move_whose_import_fails_after_the_claim_ends_the_space_where_it_was() {
    let (hub, laptop, desk, _laptop_id, desk_id) = machines("[hooks]\npost_setup = \"exit 1\"\n");
    give_origin(&laptop);
    let work = workspace(&laptop, "work");

    let moved = laptop.run(&["move", &work, "--machine", "desk", "--json"]);

    assert!(moved.status.success(), "{}", printed(&moved));
    // The hub, not the failed import, says the desk took it.
    let warning = String::from_utf8_lossy(&moved.stderr);
    assert!(
        warning.contains("the hub says machine desk took"),
        "{warning}"
    );
    listed(&hub, &work, &desk_id);
    assert!(own_spaces(&desk).iter().any(|space| space["id"] == work));
    assert!(own_spaces(&laptop).iter().all(|space| space["id"] != work));
}

#[test]
fn a_space_left_behind_keeps_the_moved_space_as_its_parent() {
    let (hub, laptop, _desk, laptop_id, desk_id) = machines("");
    give_origin(&laptop);
    let work = workspace(&laptop, "work");
    let repo = laptop.repo.to_str().unwrap().to_owned();
    laptop.ok(&["create", &repo, "--branch", "child", "--parent", &work]);
    let child = laptop.id("child");

    let moved = laptop.ok(&["move", &work, "--machine", "desk"]);

    assert_eq!(moved["failed"], json!([]), "{moved}");
    listed(&hub, &work, &desk_id);
    // The hub lists it on the laptop still, below the space on the desk.
    wait_for("the child below the moved space", || {
        let child = listed(&hub, &child, &laptop_id);
        (child["parent"] == work.as_str()).then_some(())
    });
}
