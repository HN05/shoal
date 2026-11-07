use super::*;
use crate::forge::updates::{Update, UpdateKind};
use crate::time::UnixTime;

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
        created_at: UnixTime::from_seconds(0),
        message: text.into(),
        attachments: vec!["/repo/.attachments/w/k/0/shot.png".into()],
    };
    let updates = Updates {
        updates: Vec::new(),
        messages: vec![message("See the crash"), message("")],
        leftovers: Vec::new(),
        timed_out: false,
        superseded: false,
    };
    let context = Inbox {
        space: "worker".into(),
        kind: "workspace",
    }
    .hook_context(&updates);
    let entries: Vec<_> = context.lines().skip(1).collect();
    assert_eq!(entries.len(), 3, "{context}");
    assert!(entries[0].ends_with("See the crash"), "{context}");
    for entry in &entries[1..] {
        assert!(
            entry.ends_with("/repo/.attachments/w/k/0/shot.png"),
            "{context}"
        );
    }
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
            created_at: UnixTime::from_seconds(0),
            message: "Stop the dev server".into(),
            attachments: Vec::new(),
        }],
        leftovers: Vec::new(),
        timed_out: false,
        superseded: false,
    };
    let context = Inbox {
        space: "worker".into(),
        kind: "workspace",
    }
    .hook_context(&updates);
    for part in [
        "- Stop the dev server\n",
        "- https://forge.example/team/repo/pulls/7 [comment]: Comments or reviews changed",
    ] {
        assert!(context.contains(part), "{part:?} in {context}");
    }
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

#[test]
fn only_prompts_the_user_submits_reply_in_the_chat() {
    let prompt = |text: &str| {
        user_prompt(
            &serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": text}).to_string(),
        )
    };
    assert!(prompt("Use SQLite."));
    assert!(!prompt(crate::terminal::WAKE_PROMPT));
    assert!(!prompt(crate::cli::terminal::CONTINUE_PROMPT));
    // A wake typed after a draft the user left in the input.
    assert!(!prompt(&format!(
        "half a thought{}\n",
        crate::terminal::WAKE_PROMPT
    )));
    assert!(!user_prompt(
        r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#
    ));
    assert!(!user_prompt(""));
}
