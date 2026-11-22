use super::*;
use crate::model::Execution;
use crate::sessions::HostName;

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
        space_id: "workspace".into(),
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
        Recorded::space(&executions, &places),
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
        space_id: "workspace".into(),
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
        Recorded::space(&[execution], &[]),
        [Recorded {
            place: &place,
            agent: Some(&"claude".into()),
            execution: Some(&"agent".into()),
            stopped: Some(&protection),
        }]
    );
}

#[test]
fn every_record_of_a_stopped_agents_place_reports_the_stop_and_its_execution() {
    let execution = Origin::created(HostName::Zmx, "work".to_owned());
    let created = Origin {
        instance: Some("1:10".into()),
        ..execution.clone()
    };
    let other = Origin::created(HostName::Zmx, "other".to_owned());
    // A host may give other places the same name, such as tmux panes named
    // after their windows: another agent's and a shell keep their own state.
    let running = Origin::created(HostName::Zmx, "running".to_owned());
    let shell = Origin::created(HostName::Zmx, "shell".to_owned());
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
        Session::open("work".into(), 0, &shell),
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
            (false, Some(ExecutionId::from("running"))),
            (false, None)
        ]
    );
}

#[test]
fn agent_places_count_stopped_agents_as_connected() {
    let place = |name: &str| Origin::created(HostName::Zmx, name.into());
    let (running, stopped, gone) = (place("running"), place("stopped"), place("gone"));
    let agent = |space: &str, origin: Option<&Origin>, name: Option<&str>| RunningAgent {
        execution: format!("{space}-execution").into(),
        space_id: space.into(),
        agent: name.map(Into::into),
        restorable: true,
        state: None,
        interrupted: false,
        origin: origin.cloned(),
        stopped: None,
        restarted_from: Vec::new(),
    };
    let agents = [
        agent("w1", Some(&running), Some("claude")),
        RunningAgent {
            stopped: Some(Protection {
                reason: "critical memory pressure".into(),
                resumes_when: "an update arrives for it".into(),
            }),
            ..agent("w2", Some(&stopped), Some("claude"))
        },
        agent("w3", Some(&gone), None),
        agent("w4", None, Some("codex")),
    ];
    let [w1, w2, w3] = ["w1", "w2", "w3"].map(SpaceId::from);
    assert_eq!(
        agent_places(&agents),
        AgentPlaces {
            connected: vec![(&w1, &running), (&w2, &stopped)],
            unknown: vec![(&w3, &gone)],
        }
    );
}

#[tokio::test]
async fn sessions_follow_recorded_places_and_ask_each_host_once() -> anyhow::Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    install_session_hosts(root.path());
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        crate::test_support::isolated_test(
            root.path(),
            "terminal::recorded::tests::session_hosts_child",
        )
        .env("PATH", root.path().join("bin"))
        .output(),
    )
    .await??;
    let printed = crate::test_support::printed(&output);
    anyhow::ensure!(output.status.success(), "{printed}");
    // One listing answers for every recorded place, and Herdr is asked only
    // where it is on.
    let read = |name: &str| std::fs::read_to_string(root.path().join(name)).unwrap();
    assert_eq!(read("zmx-lists"), "list\n");
    assert_eq!(read("herdr-calls"), "pane get w9:p2\n");
    Ok(())
}

/// A zmx that lists only `retained` and counts its listings, and a Herdr
/// that records what it is asked and cannot answer.
fn install_session_hosts(root: &std::path::Path) {
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    crate::test_support::install_test_script(
        bin.join("zmx"),
        format!(
            "#!/bin/sh\n[ \"$*\" = list ] || exit 1\necho list >> '{}'\nprintf '  name=retained\\tpid=3\\tclients=0\\n'\n",
            root.join("zmx-lists").display()
        ),
    );
    crate::test_support::install_test_script(
        bin.join("herdr"),
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\necho unavailable\nexit 1\n",
            root.join("herdr-calls").display()
        ),
    );
}

#[tokio::test]
#[ignore = "isolated environment launched by the session hosts regression"]
async fn session_hosts_child() -> anyhow::Result<()> {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return Ok(());
    }
    let agent = |space: &str, name: Option<&str>, session: &str| RunningAgent {
        execution: format!("{space}-execution").into(),
        space_id: space.into(),
        agent: name.map(Into::into),
        restorable: name.is_some(),
        state: None,
        interrupted: false,
        origin: Some(Origin::created(HostName::Zmx, session.into())),
        stopped: None,
        restarted_from: Vec::new(),
    };
    // A connected agent counts as it is; executions whose wrapper is gone
    // count only while their session is open.
    let agents = [
        agent("w1", Some("claude"), "workspace-session"),
        agent("w3", None, "stale"),
        agent("w4", None, "retained"),
        agent("c1", Some("codex"), "chat-session"),
    ];
    // `w5` turned Herdr off, so its pane is never asked about; `w6`'s Herdr
    // cannot answer, which leaves zmx's answers.
    let owned: Vec<(SpaceId, Origin)> = [
        ("w2", HostName::Zmx, "retained"),
        ("w3", HostName::Zmx, "gone"),
        ("w5", HostName::Herdr, "w9:p1"),
        ("w6", HostName::Herdr, "w9:p2"),
    ]
    .into_iter()
    .map(|(space, host, place)| (space.into(), Origin::created(host, place.into())))
    .collect();
    let known = known_sessions(&agents, owned.iter().map(|(space, origin)| (space, origin)));
    let settings = ["w1", "w2", "w3", "w4", "w5", "w6"].map(|space| {
        let mut layers = crate::config::repo::ConfigLayers::default();
        layers.local_config.terminal.herdr.enabled = Some(space != "w5");
        let settings = crate::config::Config::default().resolve(&layers).unwrap();
        (SpaceId::from(space), settings)
    });
    let mut owners = known.owners;
    owners.extend(open_session_owners(&known.to_ask, &HashMap::from(settings)).await);
    let expected = ["w1", "w2", "w4", "c1"].map(SpaceId::from);
    assert_eq!(owners, HashSet::from(expected));
    Ok(())
}
