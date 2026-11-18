//! The hub's directory of every joined machine's spaces: what `shoal list`
//! and `shoal status` show of the other machines, one owner per space ID,
//! and restores of a space another machine runs.
use std::{fs, path::Path};

use serde_json::Value;

use super::support::{
    fixture::{Base, home},
    git, install_test_script, shoal_binary,
    wait::wait_for,
};

/// A machine with a registered repository, which the hub when `hub`.
pub(super) fn machine(hub: bool, repository: &str) -> Base {
    start(home(), hub, repository)
}

/// [`machine`] in `root`, which may hold a state directory already.
fn start(root: tempfile::TempDir, hub: bool, repository: &str) -> Base {
    let config = match hub {
        true => "[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n",
        false => "",
    };
    Base::start(root, config, repository, &[])
}

/// Join `machine` to `hub` as `name` through the command transport, and
/// return its ID at the hub once its link is open.
pub(super) fn join(machine: &Base, hub: &Base, name: &str) -> String {
    let connect = format!(
        "{} --state-dir {} daemon connect",
        shoal_binary(),
        hub.root.path().join("state").display()
    );
    let membership = machine.ok(&["hub", "join", "--connect", &connect, "--name", name]);
    wait_for("the link to the hub", || {
        (machine.ok(&["status"])["daemon"]["hub"]["connected"] == true).then_some(())
    });
    membership["member"].as_str().unwrap().to_owned()
}

/// The hub's own machine's ID.
fn hub_machine(hub: &Base) -> String {
    let members = hub.ok(&["hub", "ls"])["members"].clone();
    let own = (members.as_array().unwrap().iter()).find(|member| member["hub"] == true);
    own.unwrap()["id"].as_str().unwrap().to_owned()
}

/// Start a workspace on the new branch `name` and return its ID.
pub(super) fn workspace(machine: &Base, name: &str) -> String {
    let repo = machine.repo.to_str().unwrap();
    machine.ok(&["start", "--wait", repo, "--branch", name]);
    machine.id(name)
}

/// The listed space `id` of the machine `of`, once `machine` lists it.
pub(super) fn listed(machine: &Base, id: &str, of: &str) -> Value {
    wait_for("the other machine's space", || {
        let listed = machine.ok(&["list"]);
        (listed.as_array().unwrap().iter())
            .find(|space| space["id"] == id && space["machine"] == of)
            .cloned()
    })
}

/// A copy of `machine`'s state in a new home, without its membership of
/// the hub, as on another machine that joins it of its own.
fn copied(machine: &Base) -> tempfile::TempDir {
    let copy = home();
    fs::create_dir_all(copy.path().join("state")).unwrap();
    let copied = copy.path().join("state/state.db");
    let database = rusqlite::Connection::open(machine.root.path().join("state/state.db")).unwrap();
    database
        .execute("VACUUM INTO ?1", [copied.to_str().unwrap()])
        .unwrap();
    let copied = rusqlite::Connection::open(Path::new(&copied)).unwrap();
    copied.execute("DELETE FROM hub_membership", []).unwrap();
    copy
}

/// Give `machine`'s repository an origin that `other` registers a clone
/// of, which a move matches, and an `ssh` that runs its commands in
/// `other`'s home and state, as on another machine.
fn reach(machine: &Base, other: &Base) {
    let root = machine.root.path();
    git(root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
    let origin = format!(
        "file://{}",
        fs::canonicalize(root.join("origin.git")).unwrap().display()
    );
    git(&machine.repo, &["remote", "add", "origin", &origin]);
    git(&machine.repo, &["push", "-q", "-u", "origin", "HEAD:main"]);
    git(&machine.repo, &["remote", "set-head", "origin", "main"]);
    let there = other.root.path();
    git(there, &["clone", "-q", &origin, "clone"]);
    other.ok(&["repo", "add", there.join("clone").to_str().unwrap()]);
    let there = there.display();
    install_test_script(
        root.join("bin/ssh"),
        format!(
            "#!/bin/sh\n\
             [ \"$1\" = -- ] && shift\n\
             shift\n\
             cd '{there}' || exit 255\n\
             export HOME='{there}' SHOAL_STATE_DIR='{there}/state' ZMX_DIR='{there}/zmx'\n\
             exec sh -c \"$*\"\n"
        ),
    );
}

#[test]
fn joined_machines_list_each_others_spaces_and_keep_an_offline_ones() {
    let hub = machine(true, "app");
    let mut laptop = machine(false, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    let hub_id = hub_machine(&hub);
    let here = workspace(&hub, "on-hub");
    let there = workspace(&laptop, "on-laptop");
    let elsewhere = listed(&hub, &there, &laptop_id);
    assert_eq!(elsewhere["name"], "on-laptop");
    assert_eq!(elsewhere["repository"]["name"], "app");
    listed(&laptop, &here, &hub_id);
    // A machine's own spaces list as its full records.
    let own = laptop.ok(&["list"]);
    let own = (own.as_array().unwrap().iter()).find(|space| space["id"] == there);
    assert!(own.is_some_and(|space| space["machine"].is_null() && space["path"].is_string()));
    let status = hub.ok(&["status", &there]);
    assert_eq!(status["machine"], laptop_id.as_str());
    // An offline machine's spaces stay, with when the hub last saw it.
    laptop.daemon.kill();
    let row = wait_for("the offline machine's row", || {
        let output = hub.run(&["list"]);
        let rows = String::from_utf8_lossy(&output.stdout).into_owned();
        rows.lines()
            .find(|row| row.contains("on-laptop") && row.contains("laptop, seen"))
            .map(str::to_owned)
    });
    assert!(row.contains("on-laptop"), "{row}");
}

#[test]
fn a_second_machine_with_a_spaces_id_is_a_conflict_the_hub_lists_once() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    let work = workspace(&laptop, "work");
    listed(&hub, &work, &laptop_id);
    let desk = start(copied(&laptop), false, "desk-app");
    join(&desk, &hub, "desk");
    let alert = wait_for("the conflict alert", || {
        let alerts = hub.ok(&["alerts"]);
        (alerts.as_array().unwrap().iter())
            .find(|alert| alert["kind"] == "space_conflict")
            .cloned()
    });
    let message = alert["message"].as_str().unwrap();
    assert!(
        message.contains("desk") && message.contains(&work),
        "{message}"
    );
    assert!(message.contains(&format!("shoal rm {work}")), "{message}");
    let listed = hub.ok(&["list"]);
    let copies: Vec<&Value> = (listed.as_array().unwrap().iter())
        .filter(|space| space["id"] == work)
        .collect();
    assert_eq!(copies.len(), 1, "{listed}");
    assert_eq!(copies[0]["machine"], laptop_id.as_str());
}

#[test]
fn a_restore_of_a_space_another_machine_runs_is_refused() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    let work = workspace(&laptop, "work");
    laptop.ok(&["rm", "--wait", &work, "--yes"]);
    let desk = start(copied(&laptop), false, "desk-app");
    join(&desk, &hub, "desk");
    laptop.ok(&["restore", &work]);
    listed(&hub, &work, &laptop_id);
    let refused = desk.run(&["restore", &work]);
    assert!(!refused.status.success());
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("runs on machine laptop"), "{error}");
}

#[test]
fn a_move_between_joined_machines_hands_the_space_over() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    let desk = machine(false, "desk-app");
    let laptop_id = join(&laptop, &hub, "laptop");
    let desk_id = join(&desk, &hub, "desk");
    reach(&laptop, &desk);
    let work = workspace(&laptop, "work");
    listed(&hub, &work, &laptop_id);
    let moved = laptop.ok(&["move", "ssh", "desk", &work, "--shoal", shoal_binary()]);
    assert_eq!(moved["failed"], serde_json::json!([]), "{moved}");
    listed(&hub, &work, &desk_id);
    let alerts = hub.ok(&["alerts"]);
    let conflict =
        (alerts.as_array().unwrap().iter()).find(|alert| alert["kind"] == "space_conflict");
    assert!(conflict.is_none(), "{alerts}");
}
