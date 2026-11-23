use super::*;
use crate::execution::recovery::{save_record, save_session};
use shoal_protocol::executions::TrackedAgent;

/// A space whose converted last session `old` continues session `s1`, and
/// the execution `new` restoring it, which carries it.
fn carrying(paths: &Paths) -> (SpaceId, ExecutionId) {
    let (space, old, new) = (
        SpaceId::from("space"),
        ExecutionId::from("old"),
        ExecutionId::from("new"),
    );
    let last = LastSession {
        session: Some("s1".into()),
        converted: true,
        ..serde_json::from_value(serde_json::json!({
            "execution": "old",
            "agent": "claude",
            "mode": "worker",
        }))
        .unwrap()
    };
    last.write(paths, &space).unwrap();
    let restored = LastSession::path(paths, &space, &old);
    carry(paths, &space, &new, std::slice::from_ref(&restored)).unwrap();
    consume(&restored).unwrap();
    (space, new)
}

#[test]
fn a_stop_before_a_session_is_reported_keeps_the_carried_one() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let (space, new) = carrying(&paths);

    let agent = TrackedAgent::named("claude");
    let path = save_record(&paths, &space, &new, &agent, None, None).unwrap();

    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(
        record.session.as_ref().map(ToString::to_string),
        Some("s1".into())
    );
    assert!(record.converted);
}

#[test]
fn a_reported_session_replaces_the_carried_one() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let (space, new) = carrying(&paths);

    save_session(&paths, &space, &new, &"s2".into()).unwrap();
    let agent = TrackedAgent::named("claude");
    LastSession::save(&paths, &space, &agent, &new, None).unwrap();

    let sessions = LastSession::all(&paths, &space).unwrap();
    let [last] = sessions.as_slice() else {
        panic!("one last session: {sessions:?}");
    };
    assert_eq!(
        last.session.as_ref().map(ToString::to_string),
        Some("s2".into())
    );
    assert!(!last.converted);
}
