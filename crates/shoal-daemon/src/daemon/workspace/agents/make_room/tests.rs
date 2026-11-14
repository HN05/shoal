use super::*;
use crate::{daemon::workspace::agents::Turn, state::AgentState};

fn candidate() -> Candidate {
    Candidate {
        execution: "execution".into(),
        workspace_id: "space".into(),
        workspace: "space".into(),
        light: false,
        turn: Turn {
            state: AgentState::Idle,
            since: UnixTime::from_seconds(0),
            generation: 1,
            cache_expires_at: None,
        },
    }
}

fn sleeper(candidate: &Candidate, holds_slot: bool) -> Sleeper<'_> {
    Sleeper {
        candidate,
        repository: Some("repo".into()),
        holds_slot,
        priority: Priority::NORMAL,
        free: Free::Expired,
    }
}

fn waiting(needs: Room) -> Waiting {
    Waiting {
        priority: Priority::NORMAL,
        needs,
    }
}

#[test]
fn an_agent_of_a_done_space_sleeps_for_memory_but_frees_no_slot() {
    let candidate = candidate();
    let (holding, done) = (sleeper(&candidate, true), sleeper(&candidate, false));
    let machine = || waiting(Room::Slot { repository: None });
    let repository = |id: &str| {
        waiting(Room::Slot {
            repository: Some(id.into()),
        })
    };
    assert!(holding.serves(&machine()));
    assert!(holding.serves(&repository("repo")));
    assert!(!holding.serves(&repository("other")));
    assert!(!done.serves(&machine()));
    assert!(done.serves(&waiting(Room::Memory)));
    let urgent = Waiting {
        priority: Priority::LOW,
        needs: Room::Memory,
    };
    assert!(!holding.serves(&urgent), "a higher priority keeps it");
}
