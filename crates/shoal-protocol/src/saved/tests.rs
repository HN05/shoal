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

fn command(id: &str) -> SavedCommand {
    SavedCommand {
        id: id.into(),
        path: format!("{id}.command.json").into(),
        argv: vec![id.to_owned()],
    }
}

/// The IDs of the agents, last sessions and commands `saved` keeps.
fn kept(saved: &Saved) -> (Vec<&str>, Vec<&str>) {
    let sessions = saved.agents.iter().chain(&saved.sessions);
    (
        sessions.map(|session| session.id.as_str()).collect(),
        saved
            .commands
            .iter()
            .map(|command| command.id.as_str())
            .collect(),
    )
}

#[test]
fn a_restore_of_named_agents_takes_every_stopped_command() {
    let saved = Saved {
        agents: vec![session("moved"), session("earlier")],
        sessions: vec![session("last")],
        commands: vec![command("make"), command("serve")],
    };
    let ids = |ids: &[&str]| ids.iter().map(|&id| ExecutionId::from(id)).collect();

    let mut agents = saved.clone();
    agents.retain_restoring(&ids(&["moved", "last"]));
    let mut commands = saved.clone();
    commands.retain_restoring(&ids(&["make"]));
    let mut one = saved.clone();
    one.retain_execution(&"moved".into());

    assert_eq!(
        kept(&agents),
        (vec!["moved", "last"], vec!["make", "serve"])
    );
    assert_eq!(kept(&commands), (vec![], vec!["make"]));
    assert_eq!(kept(&one), (vec!["moved"], vec![]));
}
