use super::*;

fn session(id: &str) -> SavedSession {
    SavedSession {
        id: id.into(),
        path: format!("{id}.json").into(),
        agent: None,
        stopped_at: None,
        stop_reason: None,
        idle: false,
        waits_in_wrapper: false,
        origin: None,
    }
}

#[test]
fn a_last_session_alone_continues_at_once_and_stopped_agents_queue() {
    let last = Saved {
        sessions: vec![session("last")],
        ..Saved::default()
    };
    let stopped = Saved {
        agents: vec![session("stopped")],
        ..last.clone()
    };
    let here = Queuing::Here { now: false };
    assert_eq!(last.restore(here), Restore::Continue);
    assert_eq!(stopped.restore(here), Restore::Queue { now: false });
    // Without a terminal the queue starts a last session now, and stopped
    // agents only when asked.
    let daemon = Queuing::Daemon { now: false };
    assert_eq!(last.restore(daemon), Restore::Queue { now: true });
    assert_eq!(stopped.restore(daemon), Restore::Queue { now: false });
    let now = Queuing::Daemon { now: true };
    assert_eq!(stopped.restore(now), Restore::Queue { now: true });
}

#[test]
fn an_agent_its_wrapper_restores_is_not_restorable_work() {
    let mut saved = Saved {
        agents: vec![SavedSession {
            waits_in_wrapper: true,
            ..session("waiting")
        }],
        ..Saved::default()
    };
    assert!(saved.is_unfinished() && !saved.is_restorable());
    assert!(!saved.restores_outside_wrappers());
    saved.sessions.push(session("last"));
    assert!(saved.restores_outside_wrappers());
}
