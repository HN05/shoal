use super::*;

#[test]
fn hook_events_name_their_session() {
    let session = |event: serde_json::Value| session_of(&event);
    assert_eq!(
        session(serde_json::json!({
            "session_id": "0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f",
            "hook_event_name": "Stop",
        }))
        .as_ref()
        .map(AgentSessionId::as_str),
        Some("0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f")
    );
    assert_eq!(session(serde_json::json!({"session_id": "--help"})), None);
    assert_eq!(
        session(serde_json::json!({"hook_event_name": "Stop"})),
        None
    );
}

#[test]
fn turn_ends_carry_their_final_message_cut_past_the_longest_question() {
    let message = |event: serde_json::Value| final_message(&event);
    assert_eq!(
        message(serde_json::json!({"last_assistant_message": "Which one?"})).as_deref(),
        Some("Which one?")
    );
    assert_eq!(
        message(serde_json::json!({"hook_event_name": "Stop"})),
        None
    );
    let long = "é".repeat(validate::MAX_AGENT_MESSAGE_LEN);
    let cut = message(serde_json::json!({ "last_assistant_message": long })).unwrap();
    assert!(cut.len() > validate::MAX_AGENT_MESSAGE_LEN);
    assert!(cut.len() <= validate::MAX_AGENT_MESSAGE_LEN + 2);
}
