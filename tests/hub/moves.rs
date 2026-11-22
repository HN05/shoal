//! `shoal move --machine`: moving a space to another machine joined to the
//! same hub, whose claim of the space at the hub commits the move.
use std::fs;

use serde_json::{Value, json};

use super::directory::{give_origin, join, listed, machine, workspace};
use super::routing::{message_with, scope_of};
use super::support::{
    fixture::{Base, home},
    git, git_with_identity, install_test_script, printed,
    prompt::{AnswerOptions, answer},
    wait::wait_for,
};

/// A picker that records its rows and picks those whose IDs
/// `SHOAL_TEST_PICK_ID` lists, separated by spaces.
const PICKER: &str = "#!/bin/sh\ncat > \"$SHOAL_TEST_PICK_INPUT\"\nawk -F '\\t' -v ids=\" $SHOAL_TEST_PICK_ID \" 'index(ids, \" \" $1 \" \") { print }' \"$SHOAL_TEST_PICK_INPUT\"\n";

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
    // Its agent hears where it is now.
    let told = message_with(&desk, &work, "moved here from machine laptop");
    assert_eq!(told["kind"], "space_moved", "{told}");
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

#[test]
fn a_move_on_a_terminal_offers_a_joined_machine_first_then_picks_it() {
    let (hub, laptop, _desk, _laptop_id, desk_id) = machines("");
    give_origin(&laptop);
    let work = workspace(&laptop, "work");
    install_test_script(laptop.root.path().join("bin/fzf"), PICKER);
    let rows = laptop.root.path().join("picker-rows");
    let mut command = laptop.command();
    // The kind picker's first row, then the desk among the machines.
    let picked = format!("0 {desk_id}");
    command
        .env("SHOAL_TEST_PICK_INPUT", &rows)
        .env("SHOAL_TEST_PICK_ID", &picked)
        .args(["move", &work, "--yes"]);
    let terminal = AnswerOptions {
        controlling_terminal: true,
        kill_on_timeout: true,
        terminal_stdout: false,
    };

    let (moved, _) = answer(&mut command, "", terminal);

    assert!(moved.status.success(), "{}", printed(&moved));
    listed(&hub, &work, &desk_id);
    // The machine picker offers the other machines, never this one.
    let machines = fs::read_to_string(&rows).unwrap();
    assert!(machines.contains("\tdesk"), "{machines}");
    assert!(!machines.contains("\tlaptop"), "{machines}");
}

#[test]
fn a_space_s_agent_moves_the_spaces_below_it_with_no_permission() {
    let (hub, laptop, _desk, _laptop_id, desk_id) = machines("");
    give_origin(&laptop);
    let coordinator = workspace(&laptop, "coordinator");
    let repo = laptop.repo.to_str().unwrap().to_owned();
    laptop.ok(&[
        "create",
        &repo,
        "--branch",
        "child",
        "--parent",
        &coordinator,
    ]);
    let child = laptop.id("child");
    let outside = workspace(&laptop, "outside");
    let scoped = |space: &str| {
        let mut command = laptop.command();
        command.env("SHOAL_SCOPE_TOKEN", scope_of(&laptop, &coordinator));
        laptop.output(command.args(["move", space, "--machine", "desk", "--json"]))
    };

    let moved = scoped(&child);
    let refused = scoped(&outside);

    assert!(moved.status.success(), "{}", printed(&moved));
    listed(&hub, &child, &desk_id);
    assert!(!refused.status.success(), "{}", printed(&refused));
    assert!(
        own_spaces(&laptop)
            .iter()
            .any(|space| space["id"] == outside)
    );
}

#[test]
fn a_space_s_own_move_waits_for_its_agent_s_turn_then_goes() {
    let (hub, laptop, _desk, _laptop_id, desk_id) = machines("");
    give_origin(&laptop);
    let work = workspace(&laptop, "work");
    let mut command = laptop.command();
    command.env("SHOAL_SCOPE_TOKEN", scope_of(&laptop, &work));
    let output = laptop.output(command.args(["move", &work, "--machine", "desk", "--json"]));

    assert!(output.status.success(), "{}", printed(&output));
    let deferred: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(deferred["space"], work.as_str(), "{deferred}");
    // No agent runs there, so its turn has ended and the move goes.
    listed(&hub, &work, &desk_id);
}

#[test]
fn a_copy_s_agent_is_not_told_that_its_space_moved() {
    let (_hub, laptop, desk, _laptop_id, _desk_id) = machines("");
    give_origin(&laptop);
    let work = workspace(&laptop, "work");

    let copied = laptop.ok(&["move", &work, "--machine", "desk", "--keep"]);

    assert_eq!(copied["failed"], json!([]), "{copied}");
    let copy = copied["moved"][0]["arrived"]["space"]["id"]
        .as_str()
        .unwrap();
    // The import answered after it told the agents of what moved.
    let to = json!({"space": {"space": copy}});
    let alerts = desk.request(json!({"list_alerts": {"to": to, "unread_only": true, "limit": 50}}));
    let told =
        (alerts["data"].as_array().unwrap().iter()).any(|alert| alert["kind"] == "space_moved");
    assert!(!told, "{alerts}");
    assert!(own_spaces(&laptop).iter().any(|space| space["id"] == work));
}

/// A workspace of the laptop, whose ID is `laptop_id` at `hub`, with work
/// of its own, which the desk can clone the repository of, by its ID, once
/// the hub lists it.
fn laptop_work(hub: &Base, laptop: &Base, laptop_id: &str) -> String {
    give_origin(laptop);
    let work = workspace(laptop, "work");
    let path = laptop.ok(&["status", &work])["workspace"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::write(format!("{path}/notes"), "only the laptop has this\n").unwrap();
    listed(hub, &work, laptop_id);
    work
}

/// Restore on `machine` the package in `file`, which must hold `work`'s.
fn restores(machine: &Base, file: &std::path::Path, work: &str) {
    let file = file.to_str().unwrap();
    let restored = machine.ok(&["restore", "--from", file, "--no-resume"]);
    assert_eq!(restored["space"]["id"], work, "{restored}");
}

#[test]
fn a_move_to_a_file_of_another_machine_s_space_writes_it_where_the_command_runs() {
    let (hub, laptop, desk, laptop_id, _) = machines("");
    let work = laptop_work(&hub, &laptop, &laptop_id);
    let file = hub.root.path().join("work.shoal");

    let moved = hub.ok(&["move", &work, "-o", file.to_str().unwrap()]);

    assert_eq!(moved["failed"], json!([]), "{moved}");
    assert!(own_spaces(&laptop).iter().all(|space| space["id"] != work));
    restores(&desk, &file, &work);
}

#[test]
fn a_move_to_standard_output_of_another_machine_s_space_writes_it_there() {
    let (hub, laptop, desk, laptop_id, _) = machines("");
    let work = laptop_work(&hub, &laptop, &laptop_id);

    let moved = hub.output(hub.command().args(["move", &work, "-o", "-"]));

    assert!(moved.status.success(), "{}", printed(&moved));
    assert!(own_spaces(&laptop).iter().all(|space| space["id"] != work));
    let file = desk.root.path().join("work.shoal");
    fs::write(&file, &moved.stdout).unwrap();
    restores(&desk, &file, &work);
}

#[test]
fn a_file_the_command_cannot_create_refuses_another_machine_s_move_before_it_starts() {
    let (hub, laptop, _, laptop_id, _) = machines("");
    let work = laptop_work(&hub, &laptop, &laptop_id);
    let missing = hub.root.path().join("missing/work.shoal");

    let refused = hub.run(&["move", &work, "-o", missing.to_str().unwrap()]);

    assert!(!refused.status.success(), "{}", printed(&refused));
    // No move of it is left recorded there, which another would wait for.
    let file = hub.root.path().join("work.shoal");
    let moved = hub.ok(&["move", &work, "-o", file.to_str().unwrap()]);
    assert_eq!(moved["failed"], json!([]), "{moved}");
}

/// Whether `machine` keeps a removal record of the space `id`.
fn keeps_removed(machine: &Base, id: &str) -> bool {
    let removed = machine.ok(&["restore"])["removed"].clone();
    (removed.as_array().unwrap().iter()).any(|record| record["workspace_id"] == id)
}

#[test]
fn a_removed_space_s_record_moves_to_a_joined_machine_where_its_restore_opens_it() {
    let (_hub, laptop, desk, _laptop_id, _desk_id) = machines("");
    give_origin(&laptop);
    let work = workspace(&laptop, "work");
    laptop.ok(&["rm", &work, "--wait", "--yes"]);

    let moved = laptop.ok(&["move", &work, "--machine", "desk"]);

    assert_eq!(moved["failed"], json!([]), "{moved}");
    assert!(!keeps_removed(&laptop, &work));
    assert!(keeps_removed(&desk, &work));
    desk.ok(&["restore", &work, "--no-resume"]);
    assert!(own_spaces(&desk).iter().any(|space| space["id"] == work));
}

#[test]
fn a_removed_space_s_record_the_destination_cannot_take_stays_on_its_machine() {
    let (_hub, laptop, desk, _laptop_id, _desk_id) = machines("");
    // Without an origin, the desk has nothing to clone the repository from.
    let work = workspace(&laptop, "work");
    laptop.ok(&["rm", &work, "--wait", "--yes"]);

    let moved = laptop.run(&["move", &work, "--machine", "desk", "--json"]);

    assert!(!moved.status.success(), "{}", printed(&moved));
    assert!(keeps_removed(&laptop, &work));
    assert!(!keeps_removed(&desk, &work));
}
