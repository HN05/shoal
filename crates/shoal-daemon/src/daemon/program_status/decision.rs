//! Which report the space's facts make: the first that holds of what waits
//! for the user, a failure, finished work, a stop, then the turn the
//! agent's hooks report for an agent that does not report itself.
use shoal_protocol::program_status::{ProgramState, ProgramStatus, Waits};

use super::facts::{Facts, Steps};
use crate::state::AgentState;

/// The report of `facts`, for an agent that `reports_itself` or not.
pub(super) fn status(facts: &Facts, reports_itself: bool) -> ProgramStatus {
    let (state, message, turn) = match shoals_own(facts) {
        Some((state, message)) => (state, message, false),
        None if reports_itself => (ProgramState::Clear, None, false),
        None => {
            let (state, message) = turn(facts.turn, &facts.progress);
            (state, message, true)
        }
    };
    ProgramStatus {
        state,
        title: facts.name.clone(),
        message,
        turn,
    }
}

/// What Shoal reports of its own records, when any holds. Finished work
/// shows between the agent's turns, so a new turn replaces it.
fn shoals_own(facts: &Facts) -> Option<(ProgramState, Option<String>)> {
    if let Some(thread) = &facts.thread {
        let state = ProgramState::Blocked {
            kind: Waits::Question,
        };
        return Some((state, Some(format!("{}: {}", thread.number, thread.about))));
    }
    if let Some(request) = &facts.approval {
        let state = ProgramState::Blocked {
            kind: Waits::Permission,
        };
        return Some((
            state,
            Some(format!("{}: {}", request.number, request.about)),
        ));
    }
    if let Some(failure) = facts.turn_failure {
        let message = format!("The agent's turn failed: {}", failure.describe());
        return Some((ProgramState::Error, Some(message)));
    }
    let between_turns = !matches!(facts.turn, Some(AgentState::Working | AgentState::Waiting));
    if let Some(summary) = facts.done.as_ref().filter(|_| between_turns) {
        let message = summary.clone().unwrap_or_else(|| "Done".into());
        return Some((ProgramState::Done, Some(message)));
    }
    if let Some(item) = facts.ready.as_ref().filter(|_| between_turns) {
        let message = match item {
            Some(url) => format!("Ready for review: {url}"),
            None => "Ready for review".into(),
        };
        return Some((ProgramState::Done, Some(message)));
    }
    let asleep = facts.asleep.as_ref();
    asleep.map(|reason| (ProgramState::Idle, Some(format!("Stopped: {reason}"))))
}

/// The turn `state` the agent's hooks report, with its `progress`.
fn turn(state: Option<AgentState>, progress: &Steps) -> (ProgramState, Option<String>) {
    match state {
        Some(AgentState::Working) => {
            let percent = (progress.total > 0)
                .then(|| u8::try_from(progress.done * 100 / progress.total).unwrap_or(100));
            let state = ProgramState::Working { progress: percent };
            (state, progress.line.clone())
        }
        Some(AgentState::Waiting) => (
            ProgramState::Blocked {
                kind: Waits::Permission,
            },
            None,
        ),
        Some(AgentState::Idle) => (ProgramState::Idle, None),
        None => (ProgramState::Clear, None),
    }
}
