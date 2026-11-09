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
    };
    let message = status_message(&status);
    assert_eq!(
        message.lines().nth(1),
        Some("Automatic cleanup failed 2 consecutive passes"),
        "{message}"
    );
}
