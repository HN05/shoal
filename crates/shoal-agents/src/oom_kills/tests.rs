use super::*;
use crate::execution::recovery;

/// The restore after the first kill waits for nothing more, and each
/// further kill before a finished turn doubles the wait, up to two hours.
#[test]
fn restores_back_off_as_kills_repeat() {
    let killed_at = UnixTime::from_seconds(1_000_000);
    let waits: Vec<u64> = (1..=7)
        .map(|kills| OomKills { kills, killed_at })
        .map(|kills| kills.restores_after().duration_since(killed_at).as_secs() / 60)
        .collect();
    assert_eq!(waits, [0, 5, 10, 20, 40, 80, 120]);
}

#[test]
fn a_finished_turn_forgets_the_kills() {
    let db = shoal_store::test_support::migrated().unwrap();
    let id = SpaceId::from("space");
    let at = UnixTime::from_seconds(1_000_000);
    assert!(record(&db, &id, &"first".into(), at).unwrap());
    // A kill counts once, whether reported or named by its record.
    assert!(!record(&db, &id, &"first".into(), at).unwrap());
    assert!(record(&db, &id, &"second".into(), at).unwrap());
    let counted = Some(OomKills {
        kills: 2,
        killed_at: at,
    });
    assert_eq!(get(&db, &id).unwrap(), counted);
    // A failed turn and a report that ends none keep them.
    follow_state(&db, &id, Some(AgentState::Working), AgentState::Idle, true).unwrap();
    follow_state(&db, &id, Some(AgentState::Idle), AgentState::Idle, false).unwrap();
    assert_eq!(get(&db, &id).unwrap(), counted);
    follow_state(&db, &id, Some(AgentState::Working), AgentState::Idle, false).unwrap();
    assert_eq!(get(&db, &id).unwrap(), None);
}

/// An agent the OOM killer ended shows its kills and that it also waits for
/// memory while recovery is on, after its backoff while that lasts.
#[test]
fn an_asleep_agent_names_its_kills_and_what_it_waits_for() {
    let killed_at = UnixTime::from_seconds(1_000_000);
    let asleep = |wakes_when: &str| Asleep {
        reason: recovery::OOM_REASON.into(),
        wakes_when: wakes_when.into(),
        keeps: Vec::new(),
        held_by: None,
    };
    let once = OomKills {
        kills: 1,
        killed_at,
    };
    let between = once.asleep(asleep("an update arrives for it"), killed_at, true);
    assert_eq!(between.reason, "OOM-killed once");
    assert_eq!(
        between.wakes_when,
        "an update arrives for it and memory recovers"
    );
    let thrice = OomKills {
        kills: 3,
        killed_at,
    };
    let mid_turn = thrice.asleep(asleep(WAKES_IN_QUEUE), killed_at, true);
    assert_eq!(mid_turn.reason, "OOM-killed 3 times");
    let after = thrice.restores_after().local_minutes();
    assert_eq!(
        mid_turn.wakes_when,
        format!("memory recovers after {after}")
    );
    // Without recovery's readings, it waits only for its backoff and its stop.
    let unread = thrice.asleep(asleep(WAKES_IN_QUEUE), killed_at, false);
    assert_eq!(unread.wakes_when, format!("{WAKES_IN_QUEUE} after {after}"));
}
