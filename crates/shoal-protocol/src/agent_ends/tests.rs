use super::*;
use serde_json::json;

/// The causes `agent_stopped` events keep in the store.
#[test]
fn causes_keep_their_persisted_shape() {
    let signal = StopCause::Signal(SessionEnd {
        signal: EndingSignal::Terminate,
        reached: SignalTarget::Agent,
    });
    let protection = StopCause::Protection {
        reason: "critical memory pressure".into(),
    };
    for (cause, stored) in [
        (
            signal,
            json!({"kind": "signal", "signal": "SIGTERM", "reached": "agent"}),
        ),
        (
            protection,
            json!({"kind": "protection", "reason": "critical memory pressure"}),
        ),
        (StopCause::IdleSleep, json!({"kind": "idle_sleep"})),
    ] {
        assert_eq!(serde_json::to_value(&cause).unwrap(), stored);
        assert_eq!(serde_json::from_value::<StopCause>(stored).unwrap(), cause);
    }
}
