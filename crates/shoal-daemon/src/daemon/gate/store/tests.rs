use super::*;
use crate::client::approvals::GateVerdict;
use crate::daemon::test_support::manager;

#[tokio::test]
async fn sessions_replace_each_other_and_denials_leave_failures_out() {
    let (_root, manager) = manager().await;
    manager
        .store
        .write(|tx| {
            let space = SpaceId::from("space");
            let session = |id: &str| Session {
                agent: Agent::unchecked("claude".into()),
                rules: "[]".into(),
                session: AgentSessionId::from(id.to_owned()),
                seen: Some("a1".into()),
            };
            let now = UnixTime::now();
            save_session(tx, &space, "change", &session("s1"), now)?;
            save_session(tx, &space, "change", &session("s2"), now)?;
            assert_eq!(session_of(tx, &space, "change")?, Some(session("s2")));
            assert_eq!(session_of(tx, &space, "issues")?, None);
            for (verdict, reason) in [
                (GateVerdict::Deny, "first"),
                (GateVerdict::Failed, "timed out"),
                (GateVerdict::Deny, "second"),
            ] {
                record_verdict(tx, &space, "push a1", verdict, reason, now)?;
            }
            record_verdict(tx, &space, "push b2", GateVerdict::Deny, "other", now)?;
            assert_eq!(denials(tx, &space, "push a1")?, (2, "second".into()));
            assert_eq!(denials(tx, &space, "push c3")?, (0, String::new()));
            Ok(())
        })
        .await
        .unwrap();
}

fn session_of(db: &Connection, space: &SpaceId, subject: &str) -> Result<Option<Session>> {
    session(db, space, subject)
}
