use super::*;
use crate::{model::Protection, sessions::HostName};

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

#[test]
fn an_attach_offers_only_the_sessions_of_the_agent_while_one_shows_it() {
    let left = Session::open(
        "work".into(),
        1,
        &Origin::created(HostName::Rex, "block:1".into()),
    );
    let running = agent_in("block:2", "running");
    let executions = |sessions: &[Session]| -> Vec<Option<ExecutionId>> {
        offered(sessions)
            .into_iter()
            .map(|session| session.execution.clone())
            .collect()
    };

    assert_eq!(
        executions(&[left.clone(), running]),
        vec![Some("running".into())]
    );
    assert_eq!(executions(&[left.clone(), left]), vec![None, None]);
}
