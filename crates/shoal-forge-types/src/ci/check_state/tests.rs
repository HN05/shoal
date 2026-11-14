use super::*;

#[test]
fn saved_states_read_in_every_casing() {
    let read = |text: &str| saved(serde_json::Value::from(text)).ok();
    for (text, state) in [
        ("Pending", CheckState::Pending),
        ("Success", CheckState::Success),
        ("SUCCESS", CheckState::Success),
        ("TIMED_OUT", CheckState::TimedOut),
        ("EXPECTED", CheckState::Expected),
        ("in_progress", CheckState::InProgress),
    ] {
        assert_eq!(read(text), Some(state));
    }
    assert_eq!(read("done"), None);
}

#[test]
fn checks_that_ran_and_failed_are_failed() {
    assert!(CheckState::Failure.failed());
    assert!(!CheckState::Cancelled.failed());
    assert!(!CheckState::Cancelled.passed());
    assert!(CheckState::Cancelled.complete());
    assert!(!CheckState::Queued.complete());
}
