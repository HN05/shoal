use super::*;

#[test]
fn saved_states_read_in_every_casing() {
    for (saved, state) in [
        ("\"Pending\"", CheckState::Pending),
        ("\"Success\"", CheckState::Success),
        ("\"SUCCESS\"", CheckState::Success),
        ("\"TIMED_OUT\"", CheckState::TimedOut),
        ("\"in_progress\"", CheckState::InProgress),
    ] {
        assert_eq!(serde_json::from_str::<CheckState>(saved).ok(), Some(state));
    }
    assert!(serde_json::from_str::<CheckState>("\"done\"").is_err());
    assert_eq!(
        serde_json::to_string(&CheckState::TimedOut).ok().as_deref(),
        Some("\"timed_out\"")
    );
}

#[test]
fn checks_that_ran_and_failed_are_failed() {
    assert!(CheckState::Failure.failed());
    assert!(!CheckState::Cancelled.failed());
    assert!(!CheckState::Cancelled.passed());
    assert!(CheckState::Cancelled.complete());
    assert!(!CheckState::Queued.complete());
}
