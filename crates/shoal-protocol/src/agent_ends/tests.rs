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

/// A SIGKILL is the OOM killer's only once the agent's scope counted a kill.
#[test]
fn a_kill_is_the_oom_killers_once_its_scope_counted_one() {
    let end = |signal| SessionEnd {
        signal,
        reached: SignalTarget::Agent,
    };
    let kill = end(EndingSignal::Kill);
    assert_eq!(StopCause::ended(kill, 1), StopCause::OomKill);
    assert_eq!(StopCause::ended(kill, 0), StopCause::Signal(kill));
    let terminate = end(EndingSignal::Terminate);
    assert_eq!(StopCause::ended(terminate, 1), StopCause::Signal(terminate));
}
