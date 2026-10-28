use super::*;

#[test]
fn session_env_uses_happy_cli_reconnect_variables() {
    let session = Session {
        id: "s1".into(),
        key: [9u8; 32],
        variant: Variant::DataKey,
        seq: 3,
        metadata_version: 1,
        agent_state_version: 2,
    };
    let env = session.env();
    assert_eq!(env[0], ("HAPPY_RECONNECT_SESSION_ID", "s1".to_owned()));
    assert_eq!(env[1].1, BASE64.encode([9u8; 32]));
    assert_eq!(env[2].1, "dataKey");
    assert_eq!(env[3].1, "3");
    assert_eq!(env[4].1, "1");
    assert_eq!(env[5].1, "2");
    assert!(!platform().is_empty());
}
