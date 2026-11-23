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
fn an_exit_points_to_restoring_only_the_spaces_it_stopped() {
    let left = Left::Spaces(vec![2.into(), 5.into()]);
    let restore = left.restore().unwrap_or_default();
    assert!(restore.ends_with("shoal resume 2 5"), "{restore}");
    assert_eq!(Left::Spaces(Vec::new()).restore(), None);
}
