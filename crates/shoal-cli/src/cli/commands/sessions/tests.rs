use super::*;
use crate::sessions::HostName;

fn agent_in(window: &str, execution: &str) -> Session {
    Session {
        agent: Some("claude".into()),
        execution: Some(execution.into()),
        ..Session::open(
            "work".into(),
            0,
            &Origin::created(HostName::Tmux, window.into()),
        )
    }
}

#[test]
fn an_agent_session_is_found_by_its_execution_whatever_its_name() {
    // Two windows of one host share the name `work`.
    let stopped = Session {
        stopped: Some(Protection {
            reason: "critical memory pressure".into(),
            resumes_when: "an update arrives for it".into(),
        }),
        ..agent_in("@1", "stopped")
    };
    let running = agent_in("@2", "running");
    let found = |sessions: Vec<Session>, execution: &str| {
        agent_session(sessions, Some(&execution.into())).map(|session| session.execution)
    };

    assert_eq!(found(vec![stopped, running.clone()], "stopped"), None);
    let resumed = vec![running, agent_in("@1", "stopped")];
    assert_eq!(found(resumed, "stopped"), Some(Some("stopped".into())));
}
