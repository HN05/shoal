//! Alerts across the hub's machines: one sequence of their numbers, and
//! the user's alerts, which every machine shows.
use std::{
    collections::HashSet,
    io::{BufRead, BufReader},
};

use serde_json::{Value, json};

use super::{
    directory::{join, machine, workspace},
    numbers::reroute,
    support::{daemon_client, fixture::Base, wait::wait_for},
};

/// Send the agent of `machine`'s space `id` an alert with `text`, as the
/// user, returning its number.
fn send(machine: &Base, id: &str, text: &str) -> i64 {
    let to = json!({"space": {"space": id}});
    let sent = machine.request(json!({"send_alert": {"to": to, "message": text}}));
    sent["data"]["alerts"][0]
        .as_i64()
        .unwrap_or_else(|| panic!("{sent}"))
}

/// The numbers of every alert `machine` keeps.
fn numbers(machine: &Base) -> Vec<i64> {
    let listed = machine
        .request(json!({"list_alerts": {"to": "every", "unread_only": false, "limit": 1000}}));
    (listed["data"]
        .as_array()
        .unwrap_or_else(|| panic!("{listed}"))
        .iter())
    .filter(|alert| alert["machine"].is_null())
    .map(|alert| alert["id"].as_i64().unwrap())
    .collect()
}

#[test]
fn alerts_a_machine_numbers_while_the_hub_is_unreachable_never_share_a_number() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    join(&laptop, &hub, "laptop");
    let (on_hub, on_laptop) = (workspace(&hub, "on-hub"), workspace(&laptop, "on-laptop"));
    reroute(&laptop, Some("false"));
    // Each machine numbers more alerts than the hub keeps spares for it,
    // so the laptop numbers above the highest it knows, which the hub's
    // machine takes too.
    for n in 0..25 {
        send(&laptop, &on_laptop, &format!("laptop {n}"));
        send(&hub, &on_hub, &format!("hub {n}"));
    }
    reroute(&laptop, None);
    let distinct = wait_for("every alert with a number of its own", || {
        let all: Vec<i64> = [&hub, &laptop].iter().flat_map(|m| numbers(m)).collect();
        let distinct: HashSet<i64> = all.iter().copied().collect();
        (all.len() >= 50 && distinct.len() == all.len()).then_some(distinct)
    });
    assert!(distinct.len() >= 50, "{distinct:?}");
}

/// Send the user an alert with `text` from `machine`'s space `id`, which
/// has no coordinator, returning its number.
fn send_up(machine: &Base, id: &str, text: &str) -> i64 {
    let sent = machine.request(json!({"send_alert": {"to": "up", "from": id, "message": text}}));
    assert_eq!(sent["data"]["user"], true, "{sent}");
    sent["data"]["alerts"][0]
        .as_i64()
        .unwrap_or_else(|| panic!("{sent}"))
}

/// The user's alerts `machine` shows.
fn users_alerts(machine: &Base) -> Vec<Value> {
    let listed =
        machine.request(json!({"list_alerts": {"to": "user", "unread_only": false, "limit": 200}}));
    listed["data"].as_array().cloned().unwrap_or_default()
}

/// The user's alert with `text` that `machine` shows, once it does.
fn shown(machine: &Base, text: &str) -> Value {
    wait_for("the alert", || {
        (users_alerts(machine).into_iter()).find(|alert| alert["message"] == text)
    })
}

#[test]
fn every_machine_shows_the_user_s_alerts_naming_the_machine_that_keeps_them() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    join(&laptop, &hub, "laptop");
    let (on_hub, on_laptop) = (workspace(&hub, "on-hub"), workspace(&laptop, "on-laptop"));
    // A follow on the hub hears of the laptop's alert as it comes.
    let follow = daemon_client::connect(hub.root.path());
    let protocol = daemon_client::protocol(hub.root.path());
    let watch = json!({"watch_alerts": {"keep_unread": true, "after": {"here": 0}}});
    daemon_client::send(
        &follow,
        &json!({"protocol": protocol, "id": 1, "method": watch}),
    );
    let mut followed = BufReader::new(&follow).lines();
    let mut next =
        || -> Value { serde_json::from_str(&followed.next().unwrap().unwrap()).unwrap() };
    assert_eq!(next()["type"], "ok");
    let from_laptop = send_up(&laptop, &on_laptop, "from the laptop");
    let from_hub = send_up(&hub, &on_hub, "from the hub");
    let there = shown(&hub, "from the laptop");
    assert_eq!(
        (&there["id"], &there["machine"]["name"]),
        (&json!(from_laptop), &json!("laptop")),
        "{there}"
    );
    let here = shown(&laptop, "from the hub");
    assert_eq!(here["id"], from_hub, "{here}");
    assert!(here["machine"]["name"].is_string(), "{here}");
    let followed = wait_for("the followed alert", || {
        let response = next();
        (response["data"]["message"] == "from the laptop").then_some(response)
    });
    assert_eq!(followed["data"]["machine"]["name"], "laptop", "{followed}");
    // The laptop's own alert is its own there.
    let own = shown(&laptop, "from the laptop");
    assert!(own["machine"].is_null(), "{own}");
}

#[test]
fn a_revoked_machine_s_alerts_leave_the_other_machines() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    join(&laptop, &hub, "laptop");
    let on_laptop = workspace(&laptop, "on-laptop");
    send_up(&laptop, &on_laptop, "from the laptop");
    shown(&hub, "from the laptop");
    hub.ok(&["hub", "revoke", "laptop"]);
    wait_for("the laptop's alert to go", || {
        let alerts = users_alerts(&hub);
        (!alerts
            .iter()
            .any(|alert| alert["message"] == "from the laptop"))
        .then_some(())
    });
}
