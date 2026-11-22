//! A joined machine's hand-over as it shuts down: its spaces continue on
//! the machine the hub places them on, under their IDs, and the machine
//! retires its copies once it is back.
use std::fs;

use serde_json::Value;

use super::directory::{give_origin, join};
use super::support::{
    fixture::{Base, home},
    install_test_script, printed,
    wait::wait_for,
    zmx::terminal_zmx,
};

/// An agent every machine has, which runs until stopped, and room to admit
/// it whatever memory the host has.
const AGENT: &str = "machine.max_heavy_agents = 4\nmachine.overload.disk.enabled = false\n\
    [agents.pi]\ncommand = ['sh', '-c', 'exec sleep 600']\n";

/// `machine`, given a terminal host whose sessions run their command in
/// place, where agents start detached.
fn hosting(machine: Base) -> Base {
    let root = machine.root.path();
    install_test_script(root.join("bin/zmx"), terminal_zmx());
    // Debug builds only open an existing ownership lock of the host.
    fs::write(root.join("zmx.shoal-ownership.lock"), "").unwrap();
    machine
}

/// The space `id` that `machine` runs itself, among its workspaces or,
/// with `chats`, its chats; none while it runs no such space.
fn own(machine: &Base, id: &str, chats: bool) -> Option<Value> {
    let listed = match chats {
        true => machine.ok(&["list", "--chats"]),
        false => machine.ok(&["list"]),
    };
    (listed.as_array().unwrap().iter())
        .find(|space| space["id"] == id && space["machine"].is_null())
        .cloned()
}

/// Whether `machine` runs the agent of its space `id`: a workspace's among
/// its executions, a chat's as the chat runs.
fn runs_agent(machine: &Base, id: &str) -> bool {
    let status = machine.ok(&["status", id]);
    let executions = status["executions"].as_array();
    status["chat"]["running"] == true
        || executions.is_some_and(|runs| runs.iter().any(|run| run["agent"] == "pi"))
}

/// Start on `machine` a space with `args` whose agent runs detached, and
/// return its ID once its agent runs.
fn running(machine: &Base, args: &[&str], name: &str) -> String {
    let started = machine.run(&[args, &["--agent", "pi", "--name", name, "--json"]].concat());
    assert!(started.status.success(), "{}", printed(&started));
    let id = machine.id(name);
    wait_for("its agent running", || {
        runs_agent(machine, &id).then_some(())
    });
    id
}

/// The hub and a laptop joined to it, which both run the agent.
fn machines() -> (Base, Base) {
    let hub = format!("{AGENT}[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n");
    let hub = hosting(Base::start(home(), &hub, "app", &[]));
    let laptop = hosting(Base::start(home(), AGENT, "app", &[]));
    give_origin(&laptop);
    join(&laptop, &hub, "laptop");
    (hub, laptop)
}

/// Send `machine`'s daemon the signal a shutdown sends, which on macOS
/// every such signal is, and wait until it exits.
fn shut_down(machine: &mut Base) {
    unsafe { libc::kill(machine.daemon.child.id() as i32, libc::SIGTERM) };
    machine.daemon.child.wait().unwrap();
}

/// The message of `machine`'s unread `daemon_stopped` alert.
fn stop_alert(machine: &Base) -> String {
    let alerts = machine.ok(&["alert", "list", "--unread"]);
    let stopped = (alerts.as_array().unwrap().iter())
        .find(|alert| alert["kind"] == "daemon_stopped")
        .unwrap_or_else(|| panic!("{alerts}"));
    stopped["message"].as_str().unwrap().to_owned()
}

#[test]
fn a_shutdown_continues_a_running_workspace_and_chat_on_another_machine() {
    let (hub, mut laptop) = machines();
    let repo = laptop.repo.to_str().unwrap().to_owned();
    let work = running(&laptop, &["start", &repo, "--branch", "work"], "work");
    let talk = running(&laptop, &["start", &repo, "--mode", "chat"], "talk");

    shut_down(&mut laptop);

    // The hub, the only other machine, continues both under their IDs and
    // restores their agents.
    for (id, chats) in [(&work, false), (&talk, true)] {
        wait_for("the space's agent running on the hub", || {
            (own(&hub, id, chats).is_some() && runs_agent(&hub, id)).then_some(())
        });
    }
    // Back, the laptop retires its copies instead of restoring them.
    laptop.restart();
    wait_for("the laptop's copies retired", || {
        (own(&laptop, &work, false).is_none() && own(&laptop, &talk, true).is_none()).then_some(())
    });
    let told = stop_alert(&laptop);
    assert!(told.contains("work") && told.contains("talk"), "{told}");
}

#[test]
fn a_space_whose_package_never_reaches_the_hub_stays_on_its_machine() {
    let (mut hub, mut laptop) = machines();
    let repo = laptop.repo.to_str().unwrap().to_owned();
    let work = running(&laptop, &["start", &repo, "--branch", "work"], "work");
    hub.daemon.kill();

    shut_down(&mut laptop);

    laptop.restart();
    assert!(own(&laptop, &work, false).is_some());
    let told = stop_alert(&laptop);
    let (stopped, handed) = told.split_once(". ").unwrap();
    // What it stopped continues here, as without a hub.
    assert!(stopped.contains("shoal resume"), "{told}");
    assert!(
        handed.contains("work") && handed.contains("stays here"),
        "{told}"
    );
}
