use super::*;

/// The spellings space logs already hold, which must read back unchanged.
const RECORDED: &[&str] = &[
    "open",
    "edit",
    "comment",
    "edit_comment",
    "close",
    "reopen",
    "merge",
    "auto_merge",
    "cancel_auto_merge",
    "update",
    "reply",
    "resolve",
    "unresolve",
    "review",
    "approve",
    "request_changes",
    "react",
    "unreact",
    "rerun",
    "cancel",
];

#[test]
fn recorded_changes_read_back_with_their_spelling() {
    for &recorded in RECORDED {
        let change: ItemChange = serde_json::from_value(recorded.into()).unwrap();
        assert_eq!(serde_json::to_value(change).unwrap(), recorded);
    }
}
