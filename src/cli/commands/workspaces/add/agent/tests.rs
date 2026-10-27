#[test]
fn plans_queued_with_codex_cli_stay_tracked() {
    let decode = |json: &str| serde_json::from_str::<super::ResolvedAddAgent>(json).unwrap();
    assert!(decode(r#"{"agent":"codex","codex_mode":"cli"}"#).tracked);
    assert!(!decode(r#"{"agent":"codex","codex_mode":"app"}"#).tracked);
    assert!(!decode(r#"{"agent":"claude","codex_mode":null}"#).tracked);
    assert!(decode(r#"{"agent":"codex","tracked":true}"#).tracked);
}
