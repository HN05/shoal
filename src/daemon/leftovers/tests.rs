use super::*;
use crate::client::space_ids::SpaceId;
use crate::time::UnixTime;

fn found(pid: u32, birth: &str) -> (Identity, Leftover) {
    let identity = Identity {
        pid,
        birth: birth.into(),
        unique: None,
    };
    let leftover = Leftover {
        pid,
        command: "sleep 300".into(),
        directory: None,
        started_at: UnixTime::from_seconds(0),
    };
    (identity, leftover)
}

fn pids(leftovers: Vec<Leftover>) -> Vec<u32> {
    leftovers.into_iter().map(|leftover| leftover.pid).collect()
}

#[test]
fn an_agent_is_told_about_each_leftover_once_while_it_runs() {
    let leftovers = Leftovers::default();
    leftovers.record(HashMap::from([(
        "w".into(),
        vec![found(1, "a"), found(2, "b")],
    )]));
    assert_eq!(
        pids(
            leftovers
                .undelivered(&SpaceId::from("w"), usize::MAX)
                .unwrap()
        ),
        [1, 2]
    );

    leftovers.delivered(&SpaceId::from("w"), &[1, 2]);
    assert!(
        leftovers
            .undelivered(&SpaceId::from("w"), usize::MAX)
            .unwrap()
            .is_empty()
    );
    assert_eq!(pids(leftovers.of(&SpaceId::from("w")).1), [1, 2]);

    // The next sweep finds a new one, and another process under a reused PID.
    leftovers.record(HashMap::from([(
        "w".into(),
        vec![found(1, "a"), found(2, "reused"), found(3, "c")],
    )]));
    assert_eq!(
        pids(
            leftovers
                .undelivered(&SpaceId::from("w"), usize::MAX)
                .unwrap()
        ),
        [2, 3]
    );
}

#[test]
fn a_sweep_forgets_the_leftovers_it_no_longer_finds() {
    let leftovers = Leftovers::default();
    leftovers.record(HashMap::from([("w".into(), vec![found(1, "a")])]));
    leftovers.delivered(&SpaceId::from("w"), &[1]);
    leftovers.record(HashMap::new());
    assert_eq!(leftovers.of(&SpaceId::from("w")), (0, Vec::new()));

    // Once it is gone, the same process found again is new.
    leftovers.record(HashMap::from([("w".into(), vec![found(1, "a")])]));
    assert_eq!(
        pids(
            leftovers
                .undelivered(&SpaceId::from("w"), usize::MAX)
                .unwrap()
        ),
        [1]
    );
}

#[test]
fn a_delivery_and_a_status_name_only_what_fits() {
    let leftovers = Leftovers::default();
    let many: Vec<_> = (1..=12).map(|pid| found(pid, "a")).collect();
    leftovers.record(HashMap::from([("w".into(), many)]));
    let (count, listed) = leftovers.of(&SpaceId::from("w"));
    assert_eq!((count, listed.len()), (12, LISTED_AT_ONCE));
    assert_eq!(
        leftovers
            .undelivered(&SpaceId::from("w"), usize::MAX)
            .unwrap()
            .len(),
        LISTED_AT_ONCE
    );
    let one = serde_json::to_vec(&found(1, "a").1).unwrap().len() + 1;
    assert_eq!(
        pids(leftovers.undelivered(&SpaceId::from("w"), 2 * one).unwrap()),
        [1, 2]
    );
    assert!(
        leftovers
            .undelivered(&SpaceId::from("w"), 0)
            .unwrap()
            .is_empty()
    );
}
