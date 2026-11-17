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
    };
    let message = status_message(&status);
    assert_eq!(
        message.lines().nth(1),
        Some("Automatic cleanup failed 2 consecutive passes"),
        "{message}"
    );
}

#[test]
fn an_exit_points_to_restoring_only_the_spaces_it_stopped() {
    let left = Left::Spaces(vec![2.into(), 5.into()]);
    let restore = left.restore().unwrap_or_default();
    assert!(restore.ends_with("shoal restore 2 5"), "{restore}");
    assert_eq!(Left::Spaces(Vec::new()).restore(), None);
}
