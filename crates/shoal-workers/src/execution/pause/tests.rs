use super::*;
use shoal_core::execution::recovery::RecordExt as _;

#[test]
fn stopped_lines_name_the_cause_and_how_to_restore() {
    let protection = Halt::Protection {
        recover: false,
        reason: "free disk space is below 2 GiB".into(),
        resumes_when: "free disk space reaches 5 GiB".into(),
    };
    let workspace = SpaceId::from("workspace-id");
    let line = protection.manual_restore(&workspace, &ExecutionId::from("id"));
    assert!(line.contains("free disk space is below 2 GiB"), "{line}");
    assert!(line.contains("free disk space reaches 5 GiB"), "{line}");
    assert!(
        line.ends_with("shoal resume workspace-id --execution id"),
        "{line}"
    );
    let stop = Halt::Pause {
        reason: Some("stopped by shoal stop".into()),
        removed: false,
    };
    let line = stop.manual_restore(&workspace, &ExecutionId::from("id"));
    assert!(line.contains("stopped by shoal stop"), "{line}");
    assert!(line.contains("--execution id"), "{line}");
    let removal = Halt::Pause {
        reason: Some("the workspace is being removed".into()),
        removed: true,
    };
    let line = removal.manual_restore(&workspace, &ExecutionId::from("id"));
    assert!(line.contains("shoal restore workspace-id"), "{line}");
    assert!(!line.contains("--execution"), "{line}");
}

#[test]
fn only_an_ended_session_kill_or_shutdown_of_an_agent_that_restores_unattended_queues_its_restore()
{
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
        signal: shoal_protocol::stop_causes::EndSignal::Hangup,
        reached: shoal_protocol::stop_causes::SignalTarget::Wrapper,
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
    let killed = Halt::Killed { oom: true };
    assert!(matches!(
        restorer(&recovery(true), &killed, false),
        Restorer::Queue(_)
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

#[test]
fn the_record_of_an_agent_the_oom_killer_ended_says_so_without_the_daemon() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let plan = crate::test_support::execution_plan(root.path());
    let recovery = recovery::Recovery {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        agent: "claude".into(),
        command: vec![],
        automatic: true,
        handoff_delivered: false,
        proxy: None,
    };
    let mode = Mode::Detached {
        log: root.path().join("log"),
    };
    let killed = Halt::Killed { oom: true };
    let path = save_paused_agent(&paths, &plan, &mode, &recovery, &killed, Restorer::User).unwrap();
    let record = recovery::Record::load(&path).unwrap().unwrap();
    assert_eq!(record.cause, Some(StopCause::OomKill));
    assert!(record.oom_killed());
}
