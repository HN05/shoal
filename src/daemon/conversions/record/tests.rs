use super::ChatRow;

#[test]
fn a_chat_rows_debug_leaves_out_its_scope_token() {
    let row: ChatRow =
        serde_json::from_value(serde_json::json!({"id": "c", "scope_token": "secret-token"}))
            .unwrap();

    let shown = format!("{row:?}");

    assert!(!shown.contains("secret-token"), "{shown}");
    assert!(shown.contains("\"c\""), "{shown}");
}
