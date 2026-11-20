use super::*;
use crate::{
    client::output::{
        batch::Failure,
        resume::{Queued, StoppedCommands},
    },
    model::Protection,
    sessions::HostName,
};

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

/// Whether an attach goes on after a resume that `resumed` reports, and
/// the error that refuses it otherwise.
fn resumed_outcome(resumed: Resumed) -> Result<(), String> {
    let name = SpaceName::from("work");
    ensure_resumed(&name, SpaceNumber::new(3), resumed).map_err(|error| format!("{error:#}"))
}

#[test]
fn an_attach_goes_on_once_the_resume_queued_the_agent() {
    assert_eq!(resumed_outcome(Resumed::Queued(Queued::default())), Ok(()));
}

#[test]
fn an_attach_refuses_a_resume_that_failed_naming_why() {
    let queued = Queued {
        failed: vec![Failure {
            workspace: "work".into(),
            error: "no terminal host can start queued agents".into(),
        }],
        ..Queued::default()
    };

    let refusal = resumed_outcome(Resumed::Queued(queued)).unwrap_err();
    assert!(
        refusal.ends_with(": no terminal host can start queued agents"),
        "{refusal}"
    );
}

#[test]
fn an_attach_refuses_a_resume_that_found_only_stopped_commands() {
    let commands = StoppedCommands {
        workspace: "work".into(),
        commands: vec![vec!["cargo".into(), "test".into()]],
    };

    assert!(resumed_outcome(Resumed::Commands(commands)).is_err());
}

#[test]
fn an_attach_refuses_an_agent_left_to_this_terminal_naming_its_focused_resume() {
    let here = Starts::Here {
        space: "space".into(),
        execution: "stopped".into(),
    };

    let refusal = resumed_outcome(Resumed::Starts(here)).unwrap_err();
    assert!(refusal.contains("shoal resume 3 --focus"), "{refusal}");
}
