//! Requests for a space another joined machine owns, which the caller's
//! daemon forwards there through the hub, and the events a space passes up
//! to a coordinator on another machine.
use serde_json::{Value, json};

use super::{
    directory::{join, listed, machine, reach, workspace},
    numbers::reroute,
    support::{daemon_client, fixture::Base, printed, shoal_binary, wait::wait_for},
};

/// What `machine`'s daemon answers the request `method` with, as the user.
fn request(machine: &Base, method: Value) -> Value {
    let root = machine.root.path();
    let protocol = daemon_client::protocol(root);
    let request = json!({"protocol": protocol, "id": 1, "method": method});
    daemon_client::exchange(root, &request)
}

/// Report that the agent of `machine`'s space `id` works, so the space
/// counts as one with an agent.
fn working(machine: &Base, id: &str) {
    let report = json!({"set_agent_state": {"workspace": id, "state": "working"}});
    let answer = request(machine, report);
    assert_ne!(answer["type"], "error", "{answer}");
}

/// A coordinator workspace on the hub with an agent, and a child below it
/// that moved to the laptop, by ID, with the laptop's.
struct Tree {
    hub: Base,
    laptop: Base,
    laptop_id: String,
    coordinator: String,
    child: String,
}

fn tree_across_machines() -> Tree {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    reach(&hub, &laptop);
    let coordinator = workspace(&hub, "coordinator");
    let repo = hub.repo.to_str().unwrap().to_owned();
    hub.ok(&[
        "start",
        "--wait",
        &repo,
        "--branch",
        "child",
        "--parent",
        &coordinator,
    ]);
    let child = hub.id("child");
    let moved = hub.ok(&["move", "ssh", "laptop", &child, "--shoal", shoal_binary()]);
    assert_eq!(moved["failed"], json!([]), "{moved}");
    let listed = listed(&hub, &child, &laptop_id);
    assert_eq!(listed["parent"], coordinator.as_str(), "{listed}");
    working(&hub, &coordinator);
    Tree {
        hub,
        laptop,
        laptop_id,
        coordinator,
        child,
    }
}

/// The scope token of a process of `machine`'s space `id`.
fn scope_of(machine: &Base, id: &str) -> String {
    let token = request(machine, json!({"issue_scope": {"space": id}}));
    token["data"]
        .as_str()
        .unwrap_or_else(|| panic!("{token}"))
        .to_owned()
}

/// The messages waiting for the agent of `machine`'s space `id`, once one
/// holds `text`.
fn message_with(machine: &Base, id: &str, text: &str) -> Value {
    wait_for("the agent's message", || {
        let messages = request(machine, json!({"agent_messages": {"space": id}}));
        let messages = messages["data"].as_array().cloned().unwrap_or_default();
        (messages.into_iter()).find(|message| {
            message["message"]
                .as_str()
                .is_some_and(|m| m.contains(text))
        })
    })
}

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
    let status = hub.ok(&["status", &there]);
    assert_eq!(status["workspace"]["name"], "Renamed there", "{status}");
    assert!(status["machine"].is_string(), "{status}");
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

#[test]
fn a_coordinator_messages_its_child_on_another_machine_and_nothing_outside_its_scope() {
    let Tree {
        hub,
        laptop,
        laptop_id,
        coordinator,
        child,
    } = tree_across_machines();
    let other = workspace(&laptop, "unrelated");
    listed(&hub, &other, &laptop_id);
    let token = scope_of(&hub, &coordinator);
    let scoped = |args: &[&str]| {
        let mut command = hub.command();
        command.env("SHOAL_SCOPE_TOKEN", &token).args(args);
        command.output().unwrap()
    };
    let sent = scoped(&[
        "messages",
        "send",
        "--space",
        &child,
        "from the coordinator",
    ]);
    assert!(sent.status.success(), "{}", printed(&sent));
    message_with(&laptop, &child, "from the coordinator");
    let refused = scoped(&["messages", "send", "--space", &other, "not yours"]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("outside this space's scope"), "{error}");
}

#[test]
fn a_child_s_question_reaches_its_coordinator_on_another_machine() {
    let Tree {
        hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    working(&laptop, &child);
    let ask = json!({"question": "Which database?", "recommendation": "SQLite"});
    let asked = request(
        &laptop,
        json!({"ask_question": {"space": child, "ask": ask}}),
    );
    assert_ne!(asked["type"], "error", "{asked}");
    message_with(&hub, &coordinator, "Which database?");
    let number = format!("{}.1", listed_number(&hub, &child));
    hub.ok(&["questions", &number, "--answer", "SQLite"]);
    message_with(&laptop, &child, "SQLite");
}

/// The number the hub lists the space `id` under.
fn listed_number(hub: &Base, id: &str) -> u64 {
    let spaces = hub.ok(&["list"]);
    let listed = (spaces.as_array().unwrap().iter()).find(|space| space["id"] == id);
    listed.unwrap_or_else(|| panic!("no space {id}: {spaces}"))["number"]
        .as_u64()
        .unwrap()
}

#[test]
fn the_hub_holds_a_message_for_an_offline_machine_and_refuses_what_cannot_wait() {
    let (hub, laptop, there) = laptop_space();
    reroute(&laptop, Some("false"));
    wait_for("the hub to see the laptop go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|member| member["name"] == "laptop");
        (laptop?["connected"] == false).then_some(())
    });
    let queued = hub.ok(&["messages", "send", "--space", &there, "while away"]);
    assert_eq!(queued["name"], "laptop", "{queued}");
    assert!(queued["expires_at"].is_number(), "{queued}");
    let refused = hub.run(&["rename", &there, "Renamed away"]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("machine laptop is offline"), "{error}");
    reroute(&laptop, None);
    message_with(&laptop, &there, "while away");
}
