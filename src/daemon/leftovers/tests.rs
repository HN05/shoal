use super::*;

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
        started_at: 0,
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
    assert_eq!(pids(leftovers.undelivered("w")), [1, 2]);

    leftovers.delivered("w", &[1, 2]);
    assert!(leftovers.undelivered("w").is_empty());
    assert_eq!(pids(leftovers.of("w")), [1, 2]);

    // The next sweep finds a new one, and another process under a reused PID.
    leftovers.record(HashMap::from([(
        "w".into(),
        vec![found(1, "a"), found(2, "reused"), found(3, "c")],
    )]));
    assert_eq!(pids(leftovers.undelivered("w")), [2, 3]);
}

#[test]
fn a_sweep_forgets_the_leftovers_it_no_longer_finds() {
    let leftovers = Leftovers::default();
    leftovers.record(HashMap::from([("w".into(), vec![found(1, "a")])]));
    leftovers.delivered("w", &[1]);
    leftovers.record(HashMap::new());
    assert!(leftovers.of("w").is_empty());

    // Once it is gone, the same process found again is new.
    leftovers.record(HashMap::from([("w".into(), vec![found(1, "a")])]));
    assert_eq!(pids(leftovers.undelivered("w")), [1]);
}
