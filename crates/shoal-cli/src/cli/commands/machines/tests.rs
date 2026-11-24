use shoal_protocol::time::UnixTime;

use super::*;

/// A request the hub holds for the machine `machine` until `expires_at`.
fn held(machine: &str, expires_at: i64) -> Queued {
    Queued {
        machine: machine.to_owned().into(),
        name: machine.to_owned(),
        last_seen: None,
        expires_at: UnixTime::from_seconds(expires_at),
    }
}

#[test]
fn held_requests_count_once_per_machine_with_the_first_to_expire() {
    let queued = [held("desk", 300), held("laptop", 100), held("desk", 200)];
    let machines: Vec<_> = (per_machine(&queued).into_iter())
        .map(|(held, count)| (held.name.as_str(), held.expires_at, count))
        .collect();
    assert_eq!(
        machines,
        [
            ("desk", UnixTime::from_seconds(200), 2),
            ("laptop", UnixTime::from_seconds(100), 1),
        ]
    );
}
