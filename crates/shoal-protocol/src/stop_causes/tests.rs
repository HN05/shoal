use super::*;

#[test]
fn a_signal_cause_keeps_the_signal_and_what_it_reached() {
    let cause = StopCause::Signal {
        signal: EndSignal::Kill,
        reached: SignalTarget::Agent,
    };
    let json = serde_json::to_value(cause).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"kind": "signal", "signal": "SIGKILL", "reached": "agent"})
    );
    assert_eq!(serde_json::from_value::<StopCause>(json).unwrap(), cause);
    assert_eq!(cause.to_string(), "SIGKILL to the agent");
}

#[test]
fn an_unknown_cause_is_refused() {
    let unknown = serde_json::json!({"kind": "tired"});
    assert!(serde_json::from_value::<StopCause>(unknown).is_err());
}
