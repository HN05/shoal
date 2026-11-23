use super::*;
use crate::forge::updates::{Update, UpdateKind};
use crate::time::UnixTime;
use shoal_protocol::alerts::Alert;

#[test]
fn attached_files_follow_the_message_text() {
    let message = |text: &str| Alert {
        message: text.into(),
        files: vec![shoal_protocol::files::SentFile {
            id: 3,
            space: "w".into(),
            name: "shot.png".into(),
            title: None,
            copy: shoal_protocol::files::KeptCopy::Here {
                path: "/repo/.attachments/w/k/0/shot.png".into(),
            },
        }],
        ..alert("")
    };
    let updates = Updates {
        updates: Vec::new(),
        alerts: vec![message("See the crash"), message("")],
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
fn hook_context_lists_alerts_and_item_activity() {
    let updates = Updates {
        updates: vec![Update {
            url: "https://forge.example/team/repo/pulls/7".into(),
            kind: UpdateKind::Comment,
            message: "Comments or reviews changed".into(),
            delivery: "one".into(),
            check: None,
            workspace: None,
        }],
        alerts: vec![alert("Stop the dev server")],
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
    assert_eq!(prompt("Use SQLite.").as_deref(), Some("Use SQLite."));
    assert!(prompt(crate::terminal::WAKE_PROMPT).is_none());
    assert!(prompt(crate::terminal::CONTINUE_PROMPT).is_none());
    // A wake typed after a draft the user left in the input.
    assert!(prompt(&format!("half a thought{}\n", crate::terminal::WAKE_PROMPT)).is_none());
    assert!(user_prompt(r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#).is_none());
    assert!(user_prompt("").is_none());
}

/// The user's alert of `text` to a space.
fn alert(text: &str) -> Alert {
    Alert {
        id: 1,
        created_at: UnixTime::from_seconds(0),
        kind: shoal_protocol::alerts::AlertKind::Message,
        sender: shoal_protocol::alerts::Sender::User,
        recipient: None,
        space: None,
        space_id: None,
        number: None,
        message: text.into(),
        read: false,
        files: Vec::new(),
        machine: None,
    }
}
