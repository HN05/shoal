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
