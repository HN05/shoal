//! Commands that select the spaces of another joined machine with
//! `--machine`, or name one there: they take what that machine last
//! reported, a stop waits at the hub while it is offline, and a resume
//! never does.
use serde_json::{Value, json};

use super::{
    handovers::{machines, running, runs_agent},
    numbers::reroute,
    support::{daemon_client, fixture::Base, printed, wait::wait_for},
};

/// The laptop's space `id` as the hub's daemon selects it by machine, once
/// `ready` holds of it.
fn reported(hub: &Base, id: &str, ready: impl Fn(&Value) -> bool) -> Value {
    let root = hub.root.path();
    let method = json!({"select_spaces": {"spaces": [], "machines": ["laptop"], "below": false}});
    let request = json!({"protocol": daemon_client::protocol(root), "id": 1, "method": method});
    wait_for("the laptop's report of the space", || {
        let selected = daemon_client::exchange(root, &request);
        let spaces = selected["data"].as_array()?.clone();
        (spaces.into_iter())
            .map(|located| located["elsewhere"]["space"].clone())
            .find(|space| space["id"] == id && ready(space))
    })
}

/// A laptop joined to the hub that runs the agent of its workspace `work`,
/// with the hub and that workspace's ID, once the hub lists it as one a
/// stop takes.
fn running_on_laptop() -> (Base, Base, String) {
    let (hub, laptop) = machines();
    let repo = laptop.repo.to_str().unwrap().to_owned();
    let work = running(&laptop, &["start", &repo, "--branch", "work"], "work");
    reported(&hub, &work, |space| space["stoppable"] == true);
    (hub, laptop, work)
}

/// Take the laptop off the hub, once the hub sees it go.
fn offline(hub: &Base, laptop: &Base) {
    reroute(laptop, Some("false"));
    wait_for("the hub to see the laptop go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|member| member["name"] == "laptop");
        (laptop?["connected"] == false).then_some(())
    });
}

#[test]
fn stop_and_resume_take_the_spaces_another_machine_reports_by_its_name() {
    let (hub, laptop, work) = running_on_laptop();

    let stopped = hub.ok(&["stop", "--machine", "laptop"]);
    assert_eq!(stopped["stopped"], true, "{stopped}");
    wait_for("the agent stopped on the laptop", || {
        (!runs_agent(&laptop, &work)).then_some(())
    });
    reported(&hub, &work, |space| space["resumable"] == "saved");

    let resumed = hub.ok(&["resume", "--machine", "laptop"]);
    let [queued] = resumed["elsewhere"].as_array().unwrap().as_slice() else {
        panic!("expected the laptop's space to resume: {resumed}");
    };
    assert_eq!(queued["machine"], "laptop", "{resumed}");
    // The laptop's queue holds the restore of the agent it stopped.
    assert_eq!(queued["queued"][0]["kind"], "restore", "{resumed}");
}

#[test]
fn an_offline_machine_holds_its_stops_at_the_hub_and_refuses_its_resumes() {
    let (hub, laptop, work) = running_on_laptop();
    offline(&hub, &laptop);

    let held = hub.run(&["stop", "--machine", "laptop"]);
    assert!(held.status.success(), "{}", printed(&held));
    let shown = printed(&held).to_string();
    assert!(shown.contains("machine laptop is offline"), "{shown}");

    let refused = hub.run(&["resume", &work]);
    assert!(!refused.status.success(), "{}", printed(&refused));
    let shown = printed(&refused).to_string();
    assert!(shown.contains("machine laptop is offline"), "{shown}");
}

#[test]
fn a_resume_by_machine_skips_an_offline_machine_s_spaces() {
    let (hub, laptop, work) = running_on_laptop();
    laptop.ok(&["stop", &work]);
    reported(&hub, &work, |space| space["resumable"] == "saved");
    offline(&hub, &laptop);

    let resumed = hub.ok(&["resume", "--machine", "laptop"]);
    let [skipped] = resumed["skipped"].as_array().unwrap().as_slice() else {
        panic!("expected the laptop's space to be skipped: {resumed}");
    };
    assert_eq!(skipped["machine"], "laptop", "{resumed}");
}

#[test]
fn a_discard_by_machine_discards_the_agents_another_machine_stopped() {
    let (hub, laptop, work) = running_on_laptop();
    laptop.ok(&["stop", &work]);
    reported(&hub, &work, |space| space["resumable"] == "saved");

    let discarded = hub.ok(&["restore", "--discard-agent", "--machine", "laptop"]);
    assert_eq!(
        discarded["discarded"].as_array().map(Vec::len),
        Some(1),
        "{discarded}"
    );
    reported(&hub, &work, |space| space["resumable"].is_null());
}
