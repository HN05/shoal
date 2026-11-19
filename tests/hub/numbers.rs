//! One sequence of space numbers across the hub's machines.
use std::{collections::HashSet, fs};

use serde_json::Value;

use super::{
    directory::{join, listed, machine, workspace},
    support::{fixture::Base, wait::wait_for},
};

/// The number of `machine`'s own space `id`.
fn number(machine: &Base, id: &str) -> Value {
    let spaces = machine.ok(&["list"]);
    let own = (spaces.as_array().unwrap().iter())
        .find(|space| space["id"] == id && space["machine"].is_null());
    own.unwrap_or_else(|| panic!("no space {id}: {spaces}"))["number"].clone()
}

/// Point `machine`'s link to the hub at `command`, or back at the command
/// it joined through for none, and wait until the link is as that leaves
/// it.
pub(super) fn reroute(machine: &Base, command: Option<&str>) {
    let file = machine.root.path().join(".config/shoal/config.toml");
    let config = fs::read_to_string(&file).unwrap();
    let (kept, _) = config.split_once("[machine.hub]").unwrap_or((&config, ""));
    let route = command.map_or(String::new(), |command| {
        format!("[machine.hub]\nconnect = [{command:?}]\n")
    });
    fs::write(&file, format!("{kept}{route}")).unwrap();
    machine.ok(&["config", "reload"]);
    wait_for("the link to the hub", || {
        let connected = machine.ok(&["status"])["daemon"]["hub"]["connected"].clone();
        (connected == command.is_none()).then_some(())
    });
}

#[test]
fn joining_renumbers_a_space_whose_number_the_hub_holds_and_says_so() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    let on_hub = workspace(&hub, "on-hub");
    let mine = workspace(&laptop, "mine");
    assert_eq!(
        (number(&hub, &on_hub), number(&laptop, &mine)),
        (1.into(), 1.into())
    );
    let laptop_id = join(&laptop, &hub, "laptop");
    let renumbered = wait_for("the renumbered space", || {
        let renumbered = number(&laptop, &mine);
        (renumbered != 1).then_some(renumbered)
    });
    let shown = wait_for("the hub's listing of the renumbered space", || {
        let shown = listed(&hub, &mine, &laptop_id)["number"].clone();
        (shown == renumbered).then_some(shown)
    });
    assert_eq!(
        (number(&hub, &on_hub), shown),
        (1.into(), renumbered.clone())
    );
    let alerts = laptop.ok(&["alert", "list", "--unread"]);
    let notice = (alerts.as_array().unwrap().iter())
        .find(|alert| alert["kind"] == "space_renumbered")
        .unwrap_or_else(|| panic!("no notice of the renumbering: {alerts}"));
    let message = notice["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("space {renumbered}")) && message.contains("not 1"),
        "{message}"
    );
}

#[test]
fn spaces_a_machine_numbers_while_the_hub_is_unreachable_never_share_a_number() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    reroute(&laptop, Some("false"));
    // Each machine numbers more spaces than the hub keeps spares for it,
    // so the laptop numbers one above its highest, which the hub's machine
    // takes too.
    let names = ["one", "two", "three", "four"];
    let theirs: Vec<String> = (names.iter())
        .map(|name| workspace(&laptop, &format!("laptop-{name}")))
        .collect();
    for name in names {
        workspace(&hub, &format!("hub-{name}"));
    }
    reroute(&laptop, None);
    let numbers = wait_for("every space with a number of its own", || {
        let spaces = hub.ok(&["list"]);
        let numbers: Vec<Value> = (spaces.as_array().unwrap().iter())
            .map(|space| space["number"].clone())
            .collect();
        let distinct: HashSet<String> = numbers.iter().map(Value::to_string).collect();
        let settled =
            (theirs.iter()).all(|id| listed(&hub, id, &laptop_id)["number"] == number(&laptop, id));
        (numbers.len() == 8 && distinct.len() == 8 && settled).then_some(numbers)
    });
    assert_eq!(numbers.len(), 8);
    let renumbered = [&hub, &laptop].map(|machine| {
        let alerts = machine.ok(&["alert", "list", "--unread"]);
        (alerts.as_array().unwrap().iter())
            .filter(|alert| alert["kind"] == "space_renumbered")
            .count()
    });
    assert!(renumbered.iter().sum::<usize>() > 0, "{renumbered:?}");
}
