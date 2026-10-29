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
        }],
        timed_out: false,
        superseded: false,
        attached: None,
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
fn hook_context_tells_the_agent_whether_the_user_is_attached() {
    let attached = |attached| Updates {
        updates: Vec::new(),
        messages: Vec::new(),
        timed_out: false,
        superseded: false,
        attached: Some(attached),
    };
    let inbox = Inbox::Workspace("worker".into());
    assert_eq!(
        inbox.hook_context(&attached(true)),
        "The user is attached to this session."
    );
    assert_eq!(
        inbox.hook_context(&attached(false)),
        "The user is not attached to this session."
    );
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
