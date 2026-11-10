use super::*;

#[test]
fn stopped_lines_name_the_cause_and_how_to_restore() {
    let protection = Halt::Protection {
        recover: false,
        reason: "free disk space is below 2 GiB".into(),
        resumes_when: "free disk space reaches 5 GiB".into(),
    };
    let line = protection.manual_restore("worker", &ExecutionId::from("id"));
    assert!(line.contains("free disk space is below 2 GiB"), "{line}");
    assert!(line.contains("free disk space reaches 5 GiB"), "{line}");
    assert!(
        line.ends_with("shoal resume worker --execution id"),
        "{line}"
    );
    let stop = Halt::Pause {
        reason: Some("stopped by shoal stop".into()),
        removed: false,
    };
    let line = stop.manual_restore("worker", &ExecutionId::from("id"));
    assert!(line.contains("stopped by shoal stop"), "{line}");
    assert!(!line.contains("shoal restore"), "{line}");
    let removal = Halt::Pause {
        reason: Some("the workspace is being removed".into()),
        removed: true,
    };
    let line = removal.manual_restore("worker", &ExecutionId::from("id"));
    assert!(line.contains("shoal restore worker"), "{line}");
}

#[test]
fn only_an_ended_session_or_shutdown_of_an_agent_that_restores_unattended_queues_its_restore() {
    let recovery = |automatic| recovery::Recovery {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        agent: "claude".into(),
        command: vec![],
        automatic,
        handoff_delivered: false,
        proxy: None,
    };
    let closed = Halt::Ended {
        reason: "its terminal closed",
    };
    assert!(matches!(
        restorer(&recovery(true), &closed, false),
        Restorer::Queue(env) if env.contains_key(crate::env::OUTPUT)
    ));
    // A restore that needs the user's choice waits for `shoal resume`.
    assert!(matches!(
        restorer(&recovery(false), &closed, false),
        Restorer::User
    ));
    let shutdown = Halt::Shutdown {
        reason: Some("the system is shutting down".into()),
    };
    assert!(matches!(
        restorer(&recovery(true), &shutdown, false),
        Restorer::Queue(_)
    ));
    assert!(matches!(
        restorer(&recovery(false), &shutdown, false),
        Restorer::User
    ));
    let stop = Halt::Pause {
        reason: Some("stopped by shoal stop".into()),
        removed: false,
    };
    assert!(matches!(
        restorer(&recovery(true), &stop, false),
        Restorer::User
    ));
}
