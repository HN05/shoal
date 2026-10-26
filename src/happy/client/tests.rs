use super::*;

#[test]
fn credentials_accept_legacy_secrets_and_data_key_public_keys() {
    let key = BASE64.encode([7u8; 32]);
    let legacy = parse_credentials(&format!(r#"{{"token":"t","secret":"{key}"}}"#)).unwrap();
    assert_eq!(legacy.token, "t");
    assert!(matches!(legacy.encryption, Encryption::Legacy(ref s) if s == &[7u8; 32]));
    let data_key = parse_credentials(&format!(
        r#"{{"token":"t","secret":null,"encryption":{{"publicKey":"{key}","machineKey":"{key}"}}}}"#
    ))
    .unwrap();
    assert!(
        matches!(data_key.encryption, Encryption::DataKey { ref public_key } if public_key == &[7u8; 32])
    );
    assert!(parse_credentials(r#"{"token":"t"}"#).is_err());
    assert!(parse_credentials(r#"{"secret":"AAAA"}"#).is_err());
    let short = BASE64.encode([1u8; 16]);
    assert!(parse_credentials(&format!(r#"{{"token":"t","secret":"{short}"}}"#)).is_err());
}

#[test]
fn server_url_prefers_settings_over_the_default() {
    if std::env::var_os(SERVER_URL_ENV).is_some() {
        return;
    }
    assert_eq!(server_url(&Value::Null), DEFAULT_SERVER_URL);
    assert_eq!(
        server_url(&json!({"serverUrl": "http://localhost:3005/"})),
        "http://localhost:3005"
    );
}

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
