use crate::time::UnixTime;
#[test]
fn terminal_notification_is_one_osc_9_sequence_without_control_characters() {
    let notification = crate::client::notifications::Notification {
        id: 1,
        created_at: UnixTime::from_seconds(0),
        workspace: Some("fix-login".into()),
        kind: crate::client::notifications::NotificationKind::AgentExited,
        message: "claude exited\x07 with code 0\n".into(),
        read: false,
        files: Vec::new(),
    };
    assert_eq!(
        super::terminal_notification(&notification),
        "\x1b]9;Shoal fix-login: claude exited with code 0\x07"
    );
}

#[test]
fn listed_notifications_name_the_files_sent_with_them() {
    let notification = crate::client::notifications::Notification {
        id: 1,
        created_at: UnixTime::from_seconds(0),
        workspace: Some("fix-login".into()),
        kind: crate::client::notifications::NotificationKind::AgentMessage,
        message: "Which layout?".into(),
        read: false,
        files: vec![crate::client::files::SentFile {
            id: 3,
            name: "a.png".into(),
            title: None,
            path: "/shoal/repo/.attachments/results/w/a.png".into(),
        }],
    };
    assert_eq!(
        super::with_files("row", &notification),
        "row\n  Attached file: /shoal/repo/.attachments/results/w/a.png"
    );
}
