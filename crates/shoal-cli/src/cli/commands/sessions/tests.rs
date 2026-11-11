use super::*;

fn agent_at(host: HostName, name: &str) -> Session {
    Session {
        agent: Some("claude".into()),
        ..Session::open(name.into(), 0, &Origin::created(host, name.into()))
    }
}

#[test]
fn an_agent_session_is_found_by_its_host_as_well_as_its_name() {
    let stopped = Session {
        stopped: Some(Protection {
            reason: "critical memory pressure".into(),
            resumes_when: "an update arrives for it".into(),
        }),
        ..agent_at(HostName::Zmx, "work")
    };
    let other_host = agent_at(HostName::Tmux, "work");
    let hosts = |sessions: Vec<Session>, key| {
        agent_session(sessions, key).map(|session| session.place.host)
    };

    // A session another host gives the same name is not the stopped one's.
    let sessions = vec![stopped, other_host.clone()];
    assert_eq!(hosts(sessions.clone(), Some((HostName::Zmx, "work"))), None);
    assert_eq!(hosts(sessions, None), Some(HostName::Tmux));
    // Once its agent runs again, it is found by that key.
    let resumed = vec![other_host, agent_at(HostName::Zmx, "work")];
    assert_eq!(
        hosts(resumed, Some((HostName::Zmx, "work"))),
        Some(HostName::Zmx)
    );
}
