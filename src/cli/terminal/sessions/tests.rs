use super::*;
use crate::model::{Execution, Workspace};

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
fn attachment_is_read_only_through_enabled_hosts() {
    let resolved = |global: &str| {
        let config = crate::config::Config::from_toml(global).unwrap();
        config
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap()
    };
    let (enabled, disabled) = (
        resolved(""),
        resolved("[terminal.herdr]\nenabled = false\n[terminal.rex]\nenabled = false\n"),
    );
    for host in [HostName::Herdr, HostName::Rex, HostName::Zmx] {
        let place = Origin::created(host, "p1".to_owned());
        assert!(
            Attachment::of(&enabled, place.clone()).is_some(),
            "{host:?}"
        );
        let read = Attachment::of(&disabled, place).is_some();
        assert_eq!(read, host == HostName::Zmx, "{host:?}");
    }
}

#[test]
fn a_repeated_place_keeps_the_agent_recorded_there() {
    let session = Origin::created(HostName::Zmx, "work".to_owned());
    let agent = Recorded {
        place: &session,
        agent: Some("claude"),
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
        model: Default::default(),
        id: origin.id().to_owned(),
        workspace_id: "workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper: None,
        child: None,
        group_id: None,
        origin: Some(origin.clone()),
        agent: agent.map(str::to_owned),
    };
    let detached = Execution {
        origin: None,
        ..execution(&agent, Some("codex"))
    };
    let inspection = Inspection {
        issue: None,
        completion: None,
        pr_cleanup: None,
        workspace: Workspace::new_record(
            "repo".into(),
            "workspace".into(),
            "/workspace".into(),
            "topic".into(),
            crate::state::WorkspaceState::Ready,
        ),
        executions: vec![
            execution(&agent, Some("claude")),
            execution(&setup, None),
            detached,
        ],
        places: vec![shell.clone()],
        ports: Vec::new(),
        resources: Vec::new(),
        simulators: Vec::new(),
    };
    assert_eq!(
        Recorded::workspace(&inspection),
        [
            Recorded {
                place: &agent,
                agent: Some("claude")
            },
            unrecorded(&setup),
            unrecorded(&shell),
        ]
    );
}

#[test]
fn a_session_label_names_its_agent() {
    let place = Origin::created(HostName::Zmx, "work".to_owned());
    let session = Session {
        agent: Some("claude".into()),
        ..Session::open("work".into(), 1, &place)
    };
    assert_eq!(session.label(), "work  claude  attached");
    assert_eq!(Session::open("work".into(), 0, &place).label(), "work");
    assert_eq!(
        serde_json::to_value(&session).unwrap()["agent"],
        serde_json::json!("claude")
    );
}

fn unrecorded(place: &Origin) -> Recorded<'_> {
    Recorded { place, agent: None }
}
