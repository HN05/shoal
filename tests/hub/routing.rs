//! Requests for a space another joined machine owns, which the caller's
//! daemon forwards there through the hub, and the events a space passes up
//! to a coordinator on another machine.
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    directory::{hub_machine, join, listed, machine, reach, workspace},
    numbers::reroute,
    support::{
        daemon_client,
        fixture::{Base, home},
        git, printed, pty, shoal_binary,
        wait::{try_wait_for, wait_for},
    },
};

/// What `machine`'s daemon answers the request `method` with, as the user.
fn request(machine: &Base, method: Value) -> Value {
    let root = machine.root.path();
    let protocol = daemon_client::protocol(root);
    let request = json!({"protocol": protocol, "id": 1, "method": method});
    daemon_client::exchange(root, &request)
}

/// What a process of `machine`'s space `id` prints for the Shoal command
/// `args`, which must succeed.
fn ok_in(machine: &Base, id: &str, args: &[&str]) -> Value {
    let mut command = machine.command();
    command.args(["run", "--workspace", id, "--", shoal_binary()]);
    machine.ok_from(&mut command, args)
}

/// Have the agent of `machine`'s space `id` ask "Which database?" in a
/// thread with its coordinator; the thread's number there.
fn ask(machine: &Base, id: &str) -> String {
    let opened = ok_in(machine, id, &["thread", "open", "Which database?"]);
    let number = opened["number"].as_str().unwrap().to_owned();
    ok_in(machine, id, &["send", &number, "Postgres or SQLite?"]);
    number
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
    tree_below_hub_config("")
}

/// [`tree_across_machines`] with the hub's global config `extra` besides
/// its role.
fn tree_below_hub_config(extra: &str) -> Tree {
    let config = format!("[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n{extra}");
    let hub = Base::start(home(), &config, "app", &[]);
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    reach(&hub, &laptop);
    let coordinator = workspace(&hub, "coordinator");
    ready_at(&laptop, &coordinator, &hub_machine(&hub));
    let repo = hub.repo.to_str().unwrap().to_owned();
    hub.ok(&[
        "create",
        &repo,
        "--branch",
        "child",
        "--parent",
        &coordinator,
    ]);
    let child = hub.id("child");
    let moved = hub.ok(&["move", "--ssh", "laptop", &child, "--shoal", shoal_binary()]);
    assert_eq!(moved["failed"], json!([]), "{moved}");
    // The laptop reports the child below the hub's coordinator.
    let below = try_wait_for(|| {
        (listed(&hub, &child, &laptop_id)["parent"] == coordinator.as_str()).then_some(())
    });
    assert!(
        below.is_some(),
        "the hub never listed the child below its coordinator: {moved}"
    );
    working(&hub, &coordinator);
    Tree {
        hub,
        laptop,
        laptop_id,
        coordinator,
        child,
    }
}

/// Wait until `machine` lists the space `id` of the machine `of` as past
/// its preparation, as a parent the hub lists must be to take a child.
pub(super) fn ready_at(machine: &Base, id: &str, of: &str) {
    wait_for("the space ready at the hub", || {
        (listed(machine, id, of)["status"] != "preparing").then_some(())
    });
}

/// The scope token of a process of `machine`'s space `id`.
pub(super) fn scope_of(machine: &Base, id: &str) -> String {
    let token = request(machine, json!({"issue_scope": {"space": id}}));
    token["data"]
        .as_str()
        .unwrap_or_else(|| panic!("{token}"))
        .to_owned()
}

/// The messages waiting for the agent of `machine`'s space `id`, once one
/// holds `text`.
pub(super) fn message_with(machine: &Base, id: &str, text: &str) -> Value {
    wait_for("the agent's message", || {
        let to = json!({"space": {"space": id}});
        let messages = request(
            machine,
            json!({"list_alerts": {"to": to, "unread_only": true, "limit": 50}}),
        );
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
    let stopped = hub.ok(&["stop", &there]);
    assert_eq!(stopped["stopped"], true, "{stopped}");
    hub.ok(&["rm", &there, "--wait", "--yes"]);
    let gone = laptop.run(&["status", &there, "--json"]);
    assert!(!gone.status.success(), "{}", printed(&gone));
}

#[test]
fn a_restart_and_its_cancel_go_to_the_machine_of_the_space() {
    let (hub, _laptop, there) = laptop_space();
    // The laptop decides the restart: no agent runs in its space.
    let restart = request(
        &hub,
        json!({"restart_agent": {"space": there, "restart": {}}}),
    );
    assert_eq!(restart["data"]["outcome"], "skipped", "{restart}");
    let cancel = request(&hub, json!({"cancel_restart": {"space": there}}));
    assert_eq!(cancel["type"], "cancelled_restart", "{cancel}");
}

#[test]
fn a_restart_names_the_machine_of_the_space_and_fails_at_once_while_it_is_offline() {
    let (hub, laptop, there) = laptop_space();
    let restarted = hub.ok(&["restart", &there]);
    let [skipped] = restarted["skipped"].as_array().unwrap().as_slice() else {
        panic!("expected the laptop to skip its space: {restarted}");
    };
    assert_eq!(skipped["machine"], "laptop", "{restarted}");
    reroute(&laptop, Some("false"));
    wait_for("the hub to see the laptop go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|member| member["name"] == "laptop");
        (laptop?["connected"] == false).then_some(())
    });
    let refused = hub.run(&["restart", &there]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    let shown = printed(&refused).to_string();
    assert!(shown.contains("machine laptop is offline"), "{shown}");
}

#[test]
fn a_command_that_needs_the_space_s_files_names_its_machine() {
    let (hub, _laptop, there) = laptop_space();
    let output = hub.run(&["files", &there]);
    assert!(!output.status.success(), "{}", printed(&output));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("laptop"), "{error}");
}

#[test]
fn the_diff_of_a_space_on_another_machine_takes_the_caller_s_colours() {
    let (hub, laptop, there) = laptop_space();
    let status = laptop.ok(&["status", &there]);
    let path = PathBuf::from(status["workspace"]["path"].as_str().unwrap());
    // Longer than a frame of the stream carries.
    let lines: String = (0..4000)
        .map(|line| format!("from the laptop {line}\n"))
        .collect();
    std::fs::write(path.join("added.txt"), &lines).unwrap();
    git(&path, &["add", "added.txt"]);
    let plain = hub.run(&["diff", &there]);
    assert!(plain.status.success(), "{}", printed(&plain));
    let text = String::from_utf8_lossy(&plain.stdout);
    let added = text
        .lines()
        .filter(|line| line.starts_with("+from the laptop "));
    assert_eq!(added.count(), 4000, "{}", printed(&plain));
    assert!(!text.contains('\x1b'), "{text}");
    let mut coloured = hub.command();
    coloured
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "color.diff")
        .env("GIT_CONFIG_VALUE_0", "always")
        .args(["diff", &there]);
    let coloured = hub.output(&mut coloured);
    assert!(coloured.status.success(), "{}", printed(&coloured));
    let text = String::from_utf8_lossy(&coloured.stdout);
    assert!(text.contains("\x1b["), "{text}");
}

#[test]
fn the_diff_of_a_space_on_another_machine_goes_to_the_caller_s_pager_on_a_terminal() {
    let (hub, laptop, there) = laptop_space();
    let status = laptop.ok(&["status", &there]);
    let path = PathBuf::from(status["workspace"]["path"].as_str().unwrap());
    std::fs::write(path.join("added.txt"), "from the laptop\n").unwrap();
    git(&path, &["add", "added.txt"]);
    let paged = hub.root.path().join("paged");
    let pager = format!(
        "printf '%s\\n' \"$LESS\" > {0}; cat >> {0}",
        paged.display()
    );
    let (_master, slave) = pty::open();
    // On a terminal of its own, which `Base::output` would replace.
    // The diff's own pager, which `pager.diff` names.
    let mut diff = (hub.command())
        .env_remove("GIT_PAGER")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "pager.diff")
        .env("GIT_CONFIG_VALUE_0", &pager)
        .env_remove("LESS")
        .stdin(std::process::Stdio::null())
        .stdout(slave)
        .args(["diff", &there])
        .spawn()
        .unwrap();
    let status = wait_for("the diff to end", || diff.try_wait().unwrap());
    assert!(status.success(), "{status}");
    let paged = std::fs::read_to_string(&paged).unwrap();
    assert!(paged.starts_with("FRX\n"), "{paged}");
    assert!(paged.contains("from the laptop"), "{paged}");
}

#[test]
fn the_user_s_terminal_checks_conflicts_of_a_space_on_another_machine() {
    let (hub, _laptop, there) = laptop_space();
    let check = hub.ok(&["conflicts", "--workspace", &there]);
    assert_eq!(check["files"], json!([]), "{check}");
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
    let sent = scoped(&["send", "--space", &child, "from the coordinator"]);
    assert!(sent.status.success(), "{}", printed(&sent));
    message_with(&laptop, &child, "from the coordinator");
    let refused = scoped(&["send", "--space", &other, "not yours"]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("outside this space's scope"), "{error}");
}

#[test]
fn a_process_acting_as_its_space_sends_and_withdraws_an_alert_on_another_machine() {
    let (hub, laptop, there) = laptop_space();
    // A chat's agent without a scope token names its chat, which only its
    // own machine has.
    let sender = workspace(&hub, "sender");
    listed(&laptop, &sender, &hub_machine(&hub));
    let to = json!({"space": {"space": there}});
    let sent = request(
        &hub,
        json!({"send_alert": {"to": to, "from": sender, "message": "from the hub"}}),
    );
    let id = sent["data"]["alerts"][0]
        .as_i64()
        .unwrap_or_else(|| panic!("{sent}"));
    let alert = message_with(&laptop, &there, "from the hub");
    assert_eq!(
        (&alert["sender"], &alert["space_id"]),
        (&json!("space"), &json!(sender)),
        "{alert}"
    );
    let withdrawn = request(
        &hub,
        json!({"withdraw_alert": {"id": id, "space": there, "from": sender}}),
    );
    assert_eq!(withdrawn["data"]["id"], id, "{withdrawn}");
}

#[test]
fn a_child_s_thread_reaches_its_coordinator_on_another_machine() {
    let Tree {
        hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    working(&laptop, &child);
    let number = ask(&laptop, &child);
    assert_eq!(
        number,
        format!("{}.1", listed_number(&hub, &child)),
        "the hub names the thread as its machine does"
    );
    message_with(&hub, &coordinator, "Which database?");
    // The coordinator replies from its own machine, which has no record of
    // the child's space.
    ok_in(&hub, &coordinator, &["send", &number, "SQLite"]);
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
    let queued = hub.ok(&["send", "--space", &there, "while away"]);
    assert_eq!(queued["name"], "laptop", "{queued}");
    assert!(queued["expires_at"].is_number(), "{queued}");
    let stop = hub.ok(&["stop", &there]);
    assert_eq!(stop["name"], "laptop", "{stop}");
    let refused = hub.run(&["rename", &there, "Renamed away"]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("machine laptop is offline"), "{error}");
    reroute(&laptop, None);
    message_with(&laptop, &there, "while away");
}

#[test]
fn a_thread_for_an_offline_coordinator_goes_on_to_the_user() {
    let hub = machine(true, "app");
    let desk = machine(false, "app");
    let config = "[machine.hub]\nescalate_after_minutes = 0\n";
    let laptop = Base::start(home(), config, "app", &[]);
    let desk_id = join(&desk, &hub, "desk");
    let laptop_id = join(&laptop, &hub, "laptop");
    reach(&desk, &laptop);
    let coordinator = workspace(&desk, "coordinator");
    ready_at(&laptop, &coordinator, &desk_id);
    let repo = desk.repo.to_str().unwrap().to_owned();
    desk.ok(&[
        "create",
        &repo,
        "--branch",
        "child",
        "--parent",
        &coordinator,
    ]);
    let child = desk.id("child");
    let moved = desk.ok(&["move", "--ssh", "laptop", &child, "--shoal", shoal_binary()]);
    assert_eq!(moved["failed"], json!([]), "{moved}");
    working(&desk, &coordinator);
    wait_for("the coordinator's agent at the hub", || {
        (listed(&hub, &coordinator, &desk_id)["agent"] == true).then_some(())
    });
    listed(&hub, &child, &laptop_id);
    working(&laptop, &child);
    reroute(&desk, Some("false"));
    wait_for("the hub to see the desk go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let desk = (members.as_array().unwrap().iter()).find(|member| member["name"] == "desk");
        (desk?["connected"] == false).then_some(())
    });
    ask(&laptop, &child);
    wait_for("the thread to reach the user", || {
        let alerts = laptop.ok(&["alert", "list", "--unread"]);
        (alerts.as_array().unwrap().iter())
            .find(|alert| alert["kind"] == "thread_posted" && alert["space_id"] == child.as_str())
            .cloned()
    });
}

#[test]
fn a_child_on_another_machine_holds_no_permission_its_parent_lacks() {
    // The hub's daemon runs while the test holds it.
    let Tree {
        hub: _hub,
        laptop,
        child,
        ..
    } = tree_below_hub_config("[permissions]\npush = \"deny\"\n");
    let status = laptop.ok(&["status", &child]);
    assert_eq!(status["permissions"]["push"]["value"], "deny", "{status}");
}

#[test]
fn removing_a_coordinator_removes_its_child_on_another_machine_first() {
    let Tree {
        hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    let removed = hub.run(&["rm", &coordinator, "--wait", "--yes"]);
    assert!(removed.status.success(), "{}", printed(&removed));
    let gone = laptop.run(&["status", &child, "--json"]);
    assert!(!gone.status.success(), "{}", printed(&gone));
}

#[test]
fn an_offline_child_keeps_its_coordinator_and_goes_once_its_machine_connects() {
    let Tree {
        hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    reroute(&laptop, Some("false"));
    wait_for("the hub to see the laptop go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|member| member["name"] == "laptop");
        (laptop?["connected"] == false).then_some(())
    });
    let refused = hub.run(&["rm", &coordinator, "--wait", "--yes"]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    hub.ok(&["status", &coordinator]);
    reroute(&laptop, None);
    wait_for("the child's removal on the laptop", || {
        let gone = laptop.run(&["status", &child, "--json"]);
        (!gone.status.success()).then_some(())
    });
}

#[test]
fn a_child_restored_on_another_machine_stays_below_its_coordinator() {
    let Tree {
        hub,
        laptop,
        laptop_id,
        coordinator,
        child,
    } = tree_across_machines();
    laptop.ok(&["rm", &child, "--wait", "--yes"]);
    let restored = laptop.ok(&["restore", &child]);
    let below = try_wait_for(|| {
        let listed = listed(&hub, &child, &laptop_id);
        (listed["parent"] == coordinator.as_str()).then_some(())
    });
    assert!(
        below.is_some(),
        "the restored child is not below its coordinator: {restored}"
    );
}

#[test]
fn stopping_a_coordinator_from_its_child_s_machine_stops_the_child_too() {
    // The hub's daemon runs while the test holds it.
    let Tree {
        hub: _hub,
        laptop,
        coordinator,
        child,
        ..
    } = tree_across_machines();
    working(&laptop, &child);
    let stopped = laptop.run(&["stop", &coordinator, "--json"]);
    assert!(stopped.status.success(), "{}", printed(&stopped));
    let reports = String::from_utf8_lossy(&stopped.stdout);
    assert_eq!(reports.matches("\"stopped\":true").count(), 2, "{reports}");
}

#[test]
fn a_stop_takes_spaces_here_and_on_another_machine_together() {
    let Tree {
        hub,
        laptop: _laptop,
        child,
        ..
    } = tree_across_machines();
    let here = workspace(&hub, "here");
    let stopped = hub.run(&["stop", &here, &child, "--skip-children", "--json"]);
    assert!(stopped.status.success(), "{}", printed(&stopped));
    let reports = String::from_utf8_lossy(&stopped.stdout);
    // One report for the space here, one for the laptop's.
    assert_eq!(reports.matches("\"stopped\":true").count(), 2, "{reports}");
}

#[test]
fn a_removal_in_a_terminal_session_takes_spaces_here_and_on_another_machine_together() {
    let (hub, laptop, there) = laptop_space();
    let here = workspace(&hub, "here");
    // A zmx session's place, which a space of this machine may own.
    let mut command = hub.command();
    command.env("ZMX_SESSION", "terminal");
    let removed = hub.output(command.args(["rm", &here, &there, "--wait", "--yes", "--json"]));
    assert!(removed.status.success(), "{}", printed(&removed));
    let gone = laptop.run(&["status", &there, "--json"]);
    assert!(!gone.status.success(), "{}", printed(&gone));
}
