use super::ChatRow;

#[test]
fn a_chat_row_keeps_its_scope_token_typed_and_out_of_its_debug() {
    let row: ChatRow =
        serde_json::from_value(serde_json::json!({"id": "c", "scope_token": "secret-token"}))
            .unwrap();

    let shown = format!("{row:?}");
    let stored = serde_json::to_value(&row).unwrap();

    assert!(!shown.contains("secret-token"), "{shown}");
    assert_eq!(stored["scope_token"], "secret-token");
    assert_eq!(stored["id"], "c");
}
