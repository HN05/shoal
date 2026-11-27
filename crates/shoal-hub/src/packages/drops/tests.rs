use serde_json::json;
use shoal_protocol::hub::{
    MemberKind,
    availability::{AvailabilityReason, Classified},
};

use super::*;
use crate::packages::tests::hub;

fn machine(name: &str, availability: Availability, last_seen: UnixTime) -> Member {
    Member {
        id: MemberId::from(name.to_owned()),
        kind: MemberKind::Machine,
        name: name.into(),
        version: None,
        joined_at: UnixTime::from_seconds(0),
        last_seen: Some(last_seen),
        connected: false,
        hub: false,
        availability: Some(Classified {
            availability,
            reason: AvailabilityReason::Set,
        }),
        capacity: None,
    }
}

fn listed(machine: &str, id: &str, number: u32, fields: serde_json::Value) -> Listed {
    let mut space = json!({
        "machine": machine, "id": id, "number": number, "name": id, "kind": "chat",
        "status": "inactive", "reported_at": 0,
    });
    space
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(space).unwrap()
}

#[test]
fn an_intermittent_or_sleeping_machine_away_for_half_an_hour_has_dropped() {
    let now = UnixTime::now();
    let long_ago = now - UNREACHABLE_FOR;
    let lately = now - Duration::from_secs(60);
    let directory = Directory {
        machines: vec![
            machine("laptop", Availability::Intermittent, long_ago),
            machine("tablet", Availability::Intermittent, lately),
            machine("desk", Availability::AlwaysOn, long_ago),
            machine("studio", Availability::AlwaysOn, long_ago),
        ],
        ..Directory::default()
    };
    let asleep = HashSet::from([MemberId::from("studio".to_owned())]);
    let dropped: Vec<&str> = (dropped(&directory, now, &asleep).into_iter())
        .map(|(machine, _)| machine.name.as_str())
        .collect();
    assert_eq!(dropped, ["laptop", "studio"]);
}

#[test]
fn a_space_had_something_to_do_with_a_working_agent_a_queued_one_or_a_held_event() {
    let directory = Directory {
        spaces: vec![
            listed("laptop", "working", 1, json!({"agent_state": "working"})),
            listed("laptop", "queued", 2, json!({"status": "queued"})),
            listed("laptop", "told", 3, json!({})),
            listed("laptop", "idle", 4, json!({})),
            listed("desk", "elsewhere", 5, json!({"agent_state": "working"})),
        ],
        ..Directory::default()
    };
    let held = HashSet::from([SpaceId::from("told".to_owned())]);
    let laptop = MemberId::from("laptop".to_owned());
    let busy: Vec<&str> = (busy_spaces(&directory, &laptop, &held).into_iter())
        .map(|listed| listed.space.id.as_str())
        .collect();
    assert_eq!(busy, ["working", "queued", "told"]);
}

#[test]
fn the_alert_says_how_far_behind_each_package_is_and_what_takes_it_over() {
    let dropped_at = UnixTime::now();
    let behind = dropped_at - Duration::from_secs(20 * 60);
    let spaces = [
        listed("laptop", "plan", 7, json!({"unsent_since": behind})),
        listed("laptop", "fix", 8, json!({})),
        listed("laptop", "spike", 9, json!({})),
    ];
    let busy: Vec<&Listed> = spaces.iter().collect();
    let kept = HashMap::from([
        (SpaceId::from("plan".to_owned()), dropped_at),
        (SpaceId::from("fix".to_owned()), dropped_at),
    ]);
    let laptop = machine("laptop", Availability::Intermittent, dropped_at);
    let away = (dropped_at, UNREACHABLE_FOR, false);
    let message = drop_message(&laptop, away, (&busy, &kept), 2);
    // Each space with a package names how far behind it is and its
    // restore; one without names none.
    assert!(message.contains("20m 0s") && message.contains("shoal restore 7"));
    assert!(message.contains("shoal restore 8"));
    assert!(!message.contains("shoal restore 9"));
    assert!(message.contains(" 2 other"));
}

#[tokio::test]
async fn each_drop_alerts_once() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop"]).await;
    let laptop = MemberId::from("laptop".to_owned());
    let (first, second) = (UnixTime::from_seconds(100), UnixTime::from_seconds(200));
    let taken = [
        hub.take_drop(&laptop, first).await.unwrap(),
        hub.take_drop(&laptop, first).await.unwrap(),
        hub.take_drop(&laptop, second).await.unwrap(),
    ];
    assert_eq!(taken, [true, false, true]);
}
