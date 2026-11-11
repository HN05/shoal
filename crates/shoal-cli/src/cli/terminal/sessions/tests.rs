use super::*;
use crate::model::Execution;

#[test]
fn a_place_repeats_only_in_the_same_host_and_server() {
    let pane = |socket: &str| Origin {
        data: [("HERDR_SOCKET_PATH".to_owned(), socket.to_owned())].into(),
        ..Origin::created(HostName::Herdr, "w1:p1".to_owned())
    };
    let (work, again, other) = (pane("/work.sock"), pane("/work.sock"), pane("/other.sock"));
    let rex = Origin::created(HostName::Rex, "w1:p1".to_owned());
    let distinct: Vec<_> = distinct_places(&[&work, &again, &other, &rex].map(unrecorded))
        .into_iter()
        .map(|recorded| recorded.place)
        .collect();
    assert_eq!(distinct, [&work, &other, &rex]);
}

#[test]
fn a_repeated_place_keeps_the_agent_recorded_there() {
    let session = Origin::created(HostName::Zmx, "work".to_owned());
    let agent = Recorded {
        place: &session,
        agent: Some(&"claude".into()),
        execution: None,
        stopped: None,
    };
    let other = Origin::created(HostName::Zmx, "other".to_owned());
    assert_eq!(
        distinct_places(&[unrecorded(&session), unrecorded(&other), agent]),
        [agent, unrecorded(&other)]
    );
}

#[test]
fn a_workspace_records_its_execution_agents_and_created_places() {
    let (agent, setup, shell) = (
        Origin::created(HostName::Zmx, "agent".to_owned()),
        Origin::created(HostName::Zmx, "setup".to_owned()),
        Origin::created(HostName::Zmx, "shell".to_owned()),
    );
    let execution = |origin: &Origin, agent: Option<&str>| Execution {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        id: origin.id().to_owned().into(),
        workspace_id: "workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper: None,
        child: None,
        group_id: None,
        origin: Some(origin.clone()),
        agent: agent.map(Into::into),
        stopped: None,
    };
    let detached = Execution {
        origin: None,
        ..execution(&agent, Some("codex"))
    };
    let executions = [
        execution(&agent, Some("claude")),
        execution(&setup, None),
        detached,
    ];
    let places = [shell.clone()];
    assert_eq!(
        Recorded::workspace(&executions, &places),
        [
            Recorded {
                place: &agent,
                agent: Some(&"claude".into()),
                execution: Some(&executions[0].id),
                stopped: None,
            },
            Recorded {
                execution: Some(&executions[1].id),
                ..unrecorded(&setup)
            },
            unrecorded(&shell),
        ]
    );
}

fn unrecorded(place: &Origin) -> Recorded<'_> {
    Recorded {
        place,
        agent: None,
        execution: None,
        stopped: None,
    }
}

fn stopped_by_load() -> Protection {
    Protection {
        reason: "critical memory pressure".into(),
        resumes_when: "an update arrives for it".into(),
    }
}

#[test]
fn a_workspace_records_where_a_stopped_agent_waits() {
    let place = Origin::created(HostName::Zmx, "agent".to_owned());
    let execution = Execution {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        id: "agent".into(),
        workspace_id: "workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper: None,
        child: None,
        group_id: None,
        origin: Some(place.clone()),
        agent: Some("claude".into()),
        stopped: Some(stopped_by_load()),
    };
    let protection = stopped_by_load();
    assert_eq!(
        Recorded::workspace(&[execution], &[]),
        [Recorded {
            place: &place,
            agent: Some(&"claude".into()),
            execution: Some(&"agent".into()),
            stopped: Some(&protection),
        }]
    );
}

#[test]
fn every_place_in_a_stopped_agents_session_reports_the_stop_and_its_execution() {
    let execution = Origin::created(HostName::Zmx, "work".to_owned());
    let created = Origin {
        instance: Some("1:10".into()),
        ..execution.clone()
    };
    let other = Origin::created(HostName::Zmx, "other".to_owned());
    // A host may give another agent's place the same name; it keeps its own state.
    let running = Origin::created(HostName::Zmx, "running".to_owned());
    let mut sessions = vec![
        Session::open("work".into(), 0, &created),
        Session {
            execution: Some("stopped".into()),
            stopped: Some(stopped_by_load()),
            ..Session::open("work".into(), 0, &execution)
        },
        Session::open("other".into(), 0, &other),
        Session {
            execution: Some("running".into()),
            ..Session::open("work".into(), 0, &running)
        },
    ];
    mark_stopped(&mut sessions);
    let stopped: Vec<_> = sessions
        .iter()
        .map(|session| (session.stopped.is_some(), session.execution.clone()))
        .collect();
    let stopped_execution = Some(ExecutionId::from("stopped"));
    assert_eq!(
        stopped,
        [
            (true, stopped_execution.clone()),
            (true, stopped_execution),
            (false, None),
            (false, Some(ExecutionId::from("running")))
        ]
    );
}
