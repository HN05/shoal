use std::ffi::OsString;

use super::{Inbox, WakeWait};

#[test]
fn a_resumed_wake_continues_in_the_inbox_it_found() {
    let resumed = |inbox: Option<Inbox>| {
        let wait = WakeWait {
            space: None,
            inbox,
            restarted: false,
        };
        let hook = ["internal", "wake", "--agent", "7"].map(OsString::from);
        wait.resume_arguments(hook.to_vec())
    };
    assert_eq!(
        resumed(Some(Inbox {
            space: "worker".into(),
            kind: "workspace"
        })),
        [
            "internal", "wake", "--agent", "7", "--space", "worker", "--resume"
        ]
    );
    assert_eq!(
        resumed(Some(Inbox {
            space: "chat".into(),
            kind: "chat"
        })),
        [
            "internal", "wake", "--agent", "7", "--space", "chat", "--resume"
        ]
    );
    // One not found yet comes from the environment again.
    assert_eq!(
        resumed(None),
        ["internal", "wake", "--agent", "7", "--resume"]
    );
    // A hook an earlier release handed over names the space alone when it
    // is handed over again, found or not.
    let handed_over = [
        "internal",
        "wake",
        "--agent",
        "7",
        "--workspace",
        "w1",
        "--resume",
    ];
    for inbox in [
        Some(Inbox {
            space: "w1".into(),
            kind: "workspace",
        }),
        None,
    ] {
        let wait = WakeWait {
            space: Some("w1".into()),
            inbox,
            restarted: true,
        };
        assert_eq!(
            wait.resume_arguments(handed_over.map(OsString::from).to_vec()),
            [
                "internal", "wake", "--agent", "7", "--resume", "--space", "w1"
            ]
        );
    }
}
