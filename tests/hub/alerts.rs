//! Alerts across the hub's machines: one sequence of their numbers, and
//! the user's alerts, which every machine shows.
use std::{
    collections::HashSet,
    io::{BufRead, BufReader},
};

use serde_json::{Value, json};

use super::{
    directory::{join, machine, reach, workspace},
    numbers::reroute,
    support::{daemon_client, fixture::Base, shoal_binary, wait::wait_for},
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

/// The user's alert with `text` that `machine` keeps itself, once `ready`
/// holds of it, or none once it has none.
fn own_once(machine: &Base, text: &str, ready: impl Fn(Option<&Value>) -> bool) -> Option<Value> {
    wait_for("the machine's own alert", || {
        let alerts = users_alerts(machine);
        let own =
            (alerts.iter()).find(|alert| alert["message"] == text && alert["machine"].is_null());
        ready(own).then(|| own.cloned())
    })
}

#[test]
fn reading_another_machine_s_alert_reads_it_where_it_is_kept() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    join(&laptop, &hub, "laptop");
    let on_laptop = workspace(&laptop, "on-laptop");
    let id = send_up(&laptop, &on_laptop, "from the laptop");
    shown(&hub, "from the laptop");
    let marked = hub.request(json!({"mark_alerts_read": {"ids": [id]}}));
    assert_ne!(marked["type"], "error", "{marked}");
    own_once(&laptop, "from the laptop", |own| {
        own.is_some_and(|alert| alert["read"] == true)
    });
}

#[test]
fn a_clear_reaches_an_offline_machine_and_leaves_what_it_recorded_since() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    join(&laptop, &hub, "laptop");
    let on_laptop = workspace(&laptop, "on-laptop");
    send_up(&laptop, &on_laptop, "before the clear");
    shown(&hub, "before the clear");
    reroute(&laptop, Some("false"));
    hub.ok(&["alert", "clear"]);
    send_up(&laptop, &on_laptop, "after the clear");
    reroute(&laptop, None);
    shown(&hub, "after the clear");
    own_once(&laptop, "before the clear", |own| own.is_none());
    assert!(own_once(&laptop, "after the clear", |own| own.is_some()).is_some());
    let shown_on_hub = users_alerts(&hub);
    assert!(
        !shown_on_hub
            .iter()
            .any(|alert| alert["message"] == "before the clear"),
        "{shown_on_hub:?}"
    );
}

#[test]
fn a_sender_that_moved_withdraws_its_alert_from_the_machine_that_keeps_it() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    join(&laptop, &hub, "laptop");
    reach(&laptop, &hub);
    let sender = workspace(&laptop, "sender");
    let id = send_up(&laptop, &sender, "from the sender");
    let moved = laptop.ok(&["move", "--ssh", "hub", &sender, "--shoal", shoal_binary()]);
    assert_eq!(moved["failed"], json!([]), "{moved}");
    shown(&hub, "from the sender");
    let withdraw = json!({"withdraw_alert": {"id": id, "from": sender}});
    // While the machine that keeps it is offline, a withdrawal fails at
    // once, naming that machine.
    reroute(&laptop, Some("false"));
    wait_for("the hub to see the laptop offline", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|m| m["name"] == "laptop")?;
        (laptop["connected"] == false).then_some(())
    });
    let refused = hub.request(withdraw.clone());
    assert_eq!(refused["type"], "error", "{refused}");
    assert!(
        refused["data"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("laptop") && m.contains("offline")),
        "{refused}"
    );
    reroute(&laptop, None);
    let withdrawn = hub.request(withdraw);
    assert_eq!(withdrawn["data"]["id"], id, "{withdrawn}");
    own_once(&laptop, "from the sender", |own| own.is_none());
}
