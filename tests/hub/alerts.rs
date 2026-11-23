//! Alerts across the hub's machines: one sequence of their numbers.
use std::collections::HashSet;

use serde_json::json;

use super::{
    directory::{join, machine, workspace},
    numbers::reroute,
    support::{fixture::Base, wait::wait_for},
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
