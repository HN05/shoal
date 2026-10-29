use super::*;

#[test]
fn stopped_lines_name_the_cause_and_only_protection_saves_it() {
    let protection = Halt::Protection {
        recover: false,
        reason: "free disk space is below 2 GiB".into(),
        resumes_when: "free disk space reaches 5 GiB".into(),
    };
    assert_eq!(
        protection.saved_reason(),
        Some("free disk space is below 2 GiB")
    );
    assert_eq!(
        protection.manual_restore("worker", "id"),
        "shoal: agent stopped: free disk space is below 2 GiB; once free disk space reaches 5 GiB, restore with shoal resume worker --execution id"
    );
    let stop = Halt::Pause {
        reason: Some("stopped by shoal stop".into()),
        removed: false,
    };
    assert_eq!(stop.saved_reason(), None);
    assert_eq!(stop.stated_reason(), ": stopped by shoal stop");
    assert_eq!(
        stop.manual_restore("worker", "id"),
        "shoal: agent stopped: stopped by shoal stop; restore with shoal resume worker --execution id"
    );
    let removal = Halt::Pause {
        reason: Some("the workspace is being removed".into()),
        removed: true,
    };
    assert_eq!(
        removal.manual_restore("worker", "id"),
        "shoal: agent stopped: the workspace is being removed; restore with shoal work restore worker, then shoal resume worker --execution id"
    );
    // A wrapper waiting to restore its agent learns of the removal only from
    // the cancellation.
    assert_eq!(
        restore_steps("worker", "id", true),
        "restore with shoal work restore worker, then shoal resume worker --execution id"
    );
    let pause = Halt::Pause {
        reason: None,
        removed: false,
    };
    assert_eq!(pause.stated_reason(), "");
}

#[test]
fn only_an_ended_session_or_shutdown_of_an_agent_that_restores_unattended_queues_its_restore() {
    let recovery = |automatic| recovery::Recovery {
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
    assert_eq!(shutdown.saved_reason(), Some("the system is shutting down"));
    let stop = Halt::Pause {
        reason: Some("stopped by shoal stop".into()),
        removed: false,
    };
    assert!(matches!(
        restorer(&recovery(true), &stop, false),
        Restorer::User
    ));
}
