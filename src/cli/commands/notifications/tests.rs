#[test]
fn terminal_notification_is_one_osc_9_sequence_without_control_characters() {
    let notification = crate::daemon::notifications::Notification {
        id: 1,
        created_at: 0,
        workspace: Some("fix-login".into()),
        kind: crate::daemon::notifications::NotificationKind::AgentExited,
        message: "claude exited\x07 with code 0\n".into(),
        read: false,
    };
    assert_eq!(
        super::terminal_notification(&notification),
        "\x1b]9;Shoal fix-login: claude exited with code 0\x07"
    );
}
