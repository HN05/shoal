use super::*;
use crate::forge::updates::{Update, UpdateKind};

#[test]
fn multi_line_messages_stay_one_entry() {
    assert_eq!(
        hanging_indent("Move commits:\n\nabc123 to api\ndef456 to cli", "  "),
        "Move commits:\n\n  abc123 to api\n  def456 to cli"
    );
}

#[test]
fn attached_files_follow_the_message_text() {
    let message = |text: &str| AgentMessage {
        id: 1,
        created_at: 0,
        message: text.into(),
        attachments: vec!["/repo/.attachments/w/k/0/shot.png".into()],
    };
    let updates = Updates {
        updates: Vec::new(),
        messages: vec![message("See the crash"), message("")],
        timed_out: false,
        superseded: false,
    };
    assert_eq!(
        Inbox::Workspace("worker".into()).hook_context(&updates),
        "New Shoal messages for this workspace:\n\
         - See the crash\n  Attached file: /repo/.attachments/w/k/0/shot.png\n\
         - Attached file: /repo/.attachments/w/k/0/shot.png"
    );
}

#[test]
fn attachments_are_checked_before_sending() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    let given = |path: &std::path::Path, title: &str| format!("{}{title}", path.display());
    assert_eq!(
        attachments(&[given(&file, ""), given(&file, "#Crash log ")]).unwrap(),
        [
            NewFile {
                path: file.clone(),
                title: None,
            },
            NewFile {
                path: file.clone(),
                title: Some("Crash log".into()),
            },
        ]
    );
    for refused in [
        given(dir.path(), ""),
        given(&dir.path().join("missing.png"), ""),
        given(&file, "#Crash\u{7}log"),
    ] {
        assert!(
            attachments(std::slice::from_ref(&refused)).is_err(),
            "{refused}"
        );
    }
}

#[test]
fn files_alone_may_be_the_message() {
    assert_eq!(message_text(None, None, true).unwrap(), "");
    assert_eq!(
        message_text(Some("Look".into()), None, true).unwrap(),
        "Look"
    );
    assert!(message_text(None, None, false).is_err());
}

#[test]
fn hook_output_names_the_event_it_answers() {
    let updates = Updates {
        updates: vec![Update {
            url: "https://forge.example/team/repo/pulls/7".into(),
            kind: UpdateKind::Comment,
            message: "Comments or reviews changed".into(),
            delivery: "one".into(),
            check: None,
            workspace: None,
        }],
        messages: vec![AgentMessage {
            id: 1,
            created_at: 0,
            message: "Stop the dev server".into(),
            attachments: Vec::new(),
        }],
        timed_out: false,
        superseded: false,
    };
    let context = Inbox::Workspace("worker".into()).hook_context(&updates);
    assert_eq!(
        context,
        "New Shoal messages for this workspace:\n- Stop the dev server\n\
             New activity on this workspace's issues and PRs:\n\
             - https://forge.example/team/repo/pulls/7 [comment]: Comments or reviews changed"
    );
    let output: serde_json::Value = serde_json::from_str(&hook_output(
        r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#,
        &context,
    ))
    .unwrap();
    assert_eq!(output["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    assert_eq!(output["hookSpecificOutput"]["additionalContext"], context);
    assert_eq!(hook_output("", &context), context);
}

#[test]
fn a_resumed_wake_continues_in_the_inbox_it_found() {
    let resumed = |inbox: Option<Inbox>| {
        let wait = WakeWait {
            inbox,
            restarted: false,
        };
        let hook = ["internal", "wake", "--agent", "7"].map(OsString::from);
        wait.resume_arguments(hook.to_vec())
    };
    assert_eq!(
        resumed(Some(Inbox::Workspace("worker".into()))),
        [
            "internal",
            "wake",
            "--agent",
            "7",
            "--workspace",
            "worker",
            "--resume"
        ]
    );
    // A chat's inbox and one not found yet come from the environment.
    for inbox in [Some(Inbox::Chat("chat".into())), None] {
        assert_eq!(
            resumed(inbox),
            ["internal", "wake", "--agent", "7", "--resume"]
        );
    }
}

#[test]
fn only_a_subagent_call_is_left_undelivered() {
    assert!(from_subagent(
        r#"{"hook_event_name":"PostToolUse","agent_id":"a1","agent_type":"general-purpose"}"#
    ));
    // A session started with `--agent` names its type, not a subagent.
    assert!(!from_subagent(
        r#"{"hook_event_name":"PostToolUse","agent_type":"reviewer"}"#
    ));
    assert!(!from_subagent(r#"{"hook_event_name":"PostToolUse"}"#));
    assert!(!from_subagent(""));
}
