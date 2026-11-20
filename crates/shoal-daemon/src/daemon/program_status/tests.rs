use super::{
    decision,
    facts::{Facts, Steps, Waiting},
};
use crate::state::{AgentState, TurnFailure};
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::{
    item_numbers::ItemNumber,
    program_status::{ProgramState, Waits},
};

fn facts() -> Facts {
    Facts {
        name: "Fix login".into(),
        thread: None,
        approval: None,
        turn_failure: None,
        done: None,
        ready: None,
        asleep: None,
        turn: None,
        progress: Steps::default(),
    }
}

fn waiting(item: u32, about: &str) -> Option<Waiting> {
    Some(Waiting {
        number: ItemNumber {
            space: SpaceNumber::new(3),
            item,
        },
        about: about.into(),
    })
}

/// The state and message `facts` report for an agent that does not report
/// itself.
fn reported(facts: &Facts) -> (ProgramState, Option<String>) {
    let status = decision::status(facts, false);
    assert_eq!(status.title, "Fix login");
    (status.state, status.message)
}

#[test]
fn a_thread_waiting_on_the_user_comes_before_everything_else() {
    let facts = Facts {
        thread: waiting(1, "Which database?"),
        approval: waiting(2, "push fix-login"),
        turn_failure: Some(TurnFailure::Other),
        done: Some(None),
        turn: Some(AgentState::Working),
        ..facts()
    };
    assert_eq!(
        reported(&facts),
        (
            ProgramState::Blocked {
                kind: Waits::Question
            },
            Some("3.1: Which database?".into())
        )
    );
    let facts = Facts {
        thread: None,
        ..facts
    };
    assert_eq!(
        reported(&facts),
        (
            ProgramState::Blocked {
                kind: Waits::Permission
            },
            Some("3.2: push fix-login".into())
        )
    );
    let facts = Facts {
        approval: None,
        turn_failure: Some(TurnFailure::RateLimited),
        ..facts
    };
    let (state, message) = reported(&facts);
    assert_eq!(state, ProgramState::Error);
    assert!(message.unwrap().ends_with("rate limited"));
}

#[test]
fn finished_work_shows_between_the_agents_turns() {
    let done = Facts {
        done: Some(Some("Fixed the login redirect".into())),
        ready: Some(None),
        turn: Some(AgentState::Idle),
        ..facts()
    };
    assert_eq!(
        reported(&done),
        (ProgramState::Done, Some("Fixed the login redirect".into()))
    );
    let ready = Facts {
        done: None,
        ready: Some(Some("https://forge.test/owner/repo/pulls/12".into())),
        ..done
    };
    let (state, message) = reported(&ready);
    assert_eq!(state, ProgramState::Done);
    assert!(
        message
            .unwrap()
            .ends_with("https://forge.test/owner/repo/pulls/12")
    );
    let working = Facts {
        turn: Some(AgentState::Working),
        ..ready
    };
    assert_eq!(
        reported(&working).0,
        ProgramState::Working { progress: None }
    );
    assert_eq!(decision::status(&working, true).state, ProgramState::Clear);
}

#[test]
fn a_stopped_agent_is_idle_with_why() {
    let facts = Facts {
        asleep: Some("blocked by #1468".into()),
        turn: Some(AgentState::Working),
        ..facts()
    };
    let (state, message) = reported(&facts);
    assert_eq!(state, ProgramState::Idle);
    assert!(message.unwrap().ends_with("blocked by #1468"));
    assert_eq!(decision::status(&facts, true).state, ProgramState::Idle);
}

#[test]
fn the_hooks_turn_shows_only_for_an_agent_that_does_not_report_itself() {
    let working = Facts {
        turn: Some(AgentState::Working),
        progress: Steps {
            line: Some("Reproduced the redirect".into()),
            done: 1,
            total: 4,
        },
        ..facts()
    };
    let status = decision::status(&working, false);
    assert_eq!(status.state, ProgramState::Working { progress: Some(25) });
    assert_eq!(status.message.as_deref(), Some("Reproduced the redirect"));
    assert!(status.turn);
    let waiting = Facts {
        turn: Some(AgentState::Waiting),
        ..working
    };
    assert_eq!(
        reported(&waiting).0,
        ProgramState::Blocked {
            kind: Waits::Permission
        }
    );
    let idle = Facts {
        turn: Some(AgentState::Idle),
        ..waiting
    };
    assert_eq!(reported(&idle).0, ProgramState::Idle);
    let status = decision::status(&idle, true);
    assert_eq!(status.state, ProgramState::Clear);
    assert!(!status.turn);
    assert_eq!(reported(&facts()).0, ProgramState::Clear);
}
