use super::*;

#[test]
fn status_names_failing_cleanup() {
    let status = crate::protocol::DaemonStatus {
        pid: 42,
        version: "1.0.0".into(),
        uptime_secs: 1,
        managed: true,
        unread_notifications: 0,
        access_stalls: Vec::new(),
        cleanup_error: Some("Automatic cleanup failed 2 consecutive passes".into()),
        hub: None,
        paused: None,
        work_memory: NO_LIMIT,
    };
    let message = status_message(&status);
    assert_eq!(
        message.lines().nth(1),
        Some("Automatic cleanup failed 2 consecutive passes"),
        "{message}"
    );
}

#[test]
fn status_names_why_the_link_to_the_hub_is_down() {
    let membership = shoal_protocol::hub::Membership {
        member: "m1".to_owned().into(),
        name: "laptop".into(),
        through: shoal_protocol::hub::Route::Command(vec!["ssh".into(), "desk".into()]),
        joined_at: crate::time::UnixTime::from_seconds(0),
    };
    let status = crate::protocol::DaemonStatus {
        pid: 42,
        version: "1.0.0".into(),
        uptime_secs: 1,
        managed: true,
        unread_notifications: 0,
        access_stalls: Vec::new(),
        cleanup_error: None,
        hub: Some(shoal_protocol::hub::HubRole::Member {
            membership,
            route: shoal_protocol::hub::Route::Command(vec!["ssh".into(), "desk".into()]),
            connected: false,
            problem: Some("the hub revoked this machine's credential".into()),
            availability: None,
        }),
        paused: None,
        work_memory: NO_LIMIT,
    };
    let message = status_message(&status);
    let line = message.lines().nth(1).unwrap_or_default();
    assert!(line.contains("laptop (m1)"), "{message}");
    assert!(
        line.ends_with("the hub revoked this machine's credential"),
        "{message}"
    );
}

#[test]
fn status_says_since_when_the_machine_is_paused() {
    let since = crate::time::UnixTime::from_seconds(1_800_000_000);
    let status = crate::protocol::DaemonStatus {
        pid: 42,
        version: "1.0.0".into(),
        uptime_secs: 1,
        managed: true,
        unread_notifications: 0,
        access_stalls: Vec::new(),
        cleanup_error: None,
        hub: None,
        paused: Some(since),
        work_memory: NO_LIMIT,
    };
    let message = status_message(&status);
    let line = message.lines().nth(1).unwrap_or_default();
    assert!(line.contains(&since.local_minutes()), "{message}");
}

const NO_LIMIT: WorkMemoryLimit = WorkMemoryLimit {
    bytes: None,
    percent: None,
    source: MemoryLimitSource::Load,
};

#[test]
fn status_names_the_work_memory_limit_and_what_sets_it() {
    let line = |bytes, percent, source| {
        work_memory_line(&WorkMemoryLimit {
            bytes,
            percent,
            source,
        })
    };
    let from_load = line(Some(24 << 30), Some(75), MemoryLimitSource::Load);
    assert!(
        from_load.ends_with("24.0 GiB, 75% of RAM, from load"),
        "{from_load}"
    );
    let written = line(Some(8 << 30), None, MemoryLimitSource::Config);
    assert!(written.ends_with("8.0 GiB, from memory_limit"), "{written}");
    let off = line(None, None, MemoryLimitSource::Config);
    assert!(off.ends_with("none, from memory_limit"), "{off}");
    let unenforced = line(None, None, MemoryLimitSource::Load);
    assert!(unenforced.ends_with("none"), "{unenforced}");
}

#[test]
fn an_exit_points_to_restoring_only_the_spaces_it_stopped() {
    let left = Left::Spaces(vec![2.into(), 5.into()]);
    let restore = left.restore().unwrap_or_default();
    assert!(restore.ends_with("shoal resume 2 5"), "{restore}");
    assert_eq!(Left::Spaces(Vec::new()).restore(), None);
}
