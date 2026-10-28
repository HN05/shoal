use super::*;

#[test]
fn only_explicit_missing_pane_is_absent() {
    assert!(missing(br#"{"error":{"code":"pane_not_found"}}"#));
    assert!(!missing(br#"{"error":{"code":"server_not_running"}}"#));
    assert!(!missing(b"connection reset"));
}
