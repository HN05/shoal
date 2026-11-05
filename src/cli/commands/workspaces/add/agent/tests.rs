#[test]
fn plans_queued_before_every_launch_ran_tracked_still_decode() {
    let decode = |json: &str| serde_json::from_str::<super::ResolvedAddAgent>(json).unwrap();
    for json in [
        r#"{"agent":"codex","codex_mode":"app"}"#,
        r#"{"agent":"codex","tracked":true}"#,
    ] {
        assert_eq!(decode(json).agent, Some("codex".parse().unwrap()));
    }
}
