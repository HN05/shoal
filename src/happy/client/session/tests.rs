use super::*;
use std::collections::HashMap;

#[test]
fn creation_payload_and_reconnect_share_the_account_encryption() -> Result<()> {
    let account_key = crypto_box::SecretKey::from([3u8; 32]);
    let accounts = [
        Encryption::Legacy(vec![7u8; 32]),
        Encryption::DataKey {
            public_key: account_key.public_key().as_bytes().to_vec(),
        },
    ];
    let paths = Paths::for_test("/fixture");
    let metadata = creation_metadata(
        &paths,
        Path::new("/fixture/.happy"),
        "machine-1",
        Path::new("/fixture/topic"),
        Some("topic"),
        Agent::Codex,
    );
    assert_eq!(metadata["path"], "/fixture/topic");
    assert_eq!(metadata["machineId"], "machine-1");
    assert_eq!(metadata["happyHomeDir"], "/fixture/.happy");
    assert_eq!(metadata["flavor"], "codex");
    assert_eq!(metadata["gitBranch"], "topic");
    for account in accounts {
        let prepared = PreparedSession::prepare(&account)?;
        let body = prepared.creation_payload(&metadata)?;
        Uuid::parse_str(body["tag"].as_str().context("missing tag")?)?;
        let session = prepared.decode_response(&json!({"session": {
            "id": "s1", "seq": 3, "metadataVersion": 1, "agentStateVersion": 2,
        }}))?;
        let env: HashMap<_, _> = session.env().into_iter().collect();
        assert_eq!(env.get("HAPPY_RECONNECT_SESSION_ID"), Some(&"s1".into()));
        assert_eq!(env.get("HAPPY_RECONNECT_SEQ"), Some(&"3".into()));
        assert_eq!(
            env.get("HAPPY_RECONNECT_METADATA_VERSION"),
            Some(&"1".into())
        );
        assert_eq!(
            env.get("HAPPY_RECONNECT_AGENT_STATE_VERSION"),
            Some(&"2".into())
        );
        let key = BASE64.decode(
            env.get("HAPPY_RECONNECT_ENCRYPTION_KEY")
                .context("missing key")?,
        )?;
        let variant = match account {
            Encryption::Legacy(secret) => {
                assert_eq!(key, secret);
                assert!(body["dataEncryptionKey"].is_null());
                assert_eq!(
                    env.get("HAPPY_RECONNECT_ENCRYPTION_VARIANT"),
                    Some(&"legacy".into())
                );
                Variant::Legacy
            }
            Encryption::DataKey { .. } => {
                assert_eq!(
                    env.get("HAPPY_RECONNECT_ENCRYPTION_VARIANT"),
                    Some(&"dataKey".into())
                );
                assert_eq!(
                    open_sealed_key(&body["dataEncryptionKey"], &account_key)?,
                    key
                );
                Variant::DataKey
            }
        };
        assert_eq!(decrypt_record(&body["metadata"], &key, variant)?, metadata);
        assert_eq!(
            decrypt_record(&body["agentState"], &key, variant)?,
            json!({"controlledByUser": false})
        );
    }
    Ok(())
}

fn decrypt_record(value: &Value, key: &[u8], variant: Variant) -> Result<Value> {
    let bundle = BASE64.decode(value.as_str().context("missing encrypted record")?)?;
    let plaintext = match variant {
        Variant::Legacy => {
            use crypto_secretbox::{
                XSalsa20Poly1305,
                aead::{Aead, KeyInit},
            };
            let (nonce, ciphertext) = bundle.split_at_checked(24).context("short legacy record")?;
            XSalsa20Poly1305::new_from_slice(key)?
                .decrypt(nonce.into(), ciphertext)
                .context("decrypt legacy record")?
        }
        Variant::DataKey => {
            use aes_gcm::{
                Aes256Gcm,
                aead::{Aead, KeyInit},
            };
            let (version, bundle) = bundle.split_first().context("missing record version")?;
            assert_eq!(*version, 0);
            let (nonce, ciphertext) = bundle
                .split_at_checked(12)
                .context("short data-key record")?;
            Aes256Gcm::new_from_slice(key)?
                .decrypt(nonce.try_into()?, ciphertext)
                .context("decrypt data-key record")?
        }
    };
    Ok(serde_json::from_slice(&plaintext)?)
}

fn open_sealed_key(value: &Value, account: &crypto_box::SecretKey) -> Result<Vec<u8>> {
    use crypto_box::{PublicKey, SalsaBox, aead::Aead};
    let bundle = BASE64.decode(value.as_str().context("missing sealed key")?)?;
    let (version, bundle) = bundle.split_first().context("missing key version")?;
    assert_eq!(*version, 0);
    let (public_key, bundle) = bundle.split_at_checked(32).context("missing public key")?;
    let public_key: [u8; 32] = public_key.try_into()?;
    let (nonce, ciphertext) = bundle.split_at_checked(24).context("missing nonce")?;
    SalsaBox::new(&PublicKey::from(public_key), account)
        .decrypt(nonce.into(), ciphertext)
        .context("open session key")
}

#[test]
fn creation_response_preserves_defaults_and_requires_a_session_id() -> Result<()> {
    let account = Encryption::Legacy(vec![7u8; 32]);
    for raw in [
        json!({"id": "s1"}),
        json!({"id": "s1", "seq": null, "metadataVersion": "1", "agentStateVersion": false}),
    ] {
        let session =
            PreparedSession::prepare(&account)?.decode_response(&json!({"session": raw}))?;
        assert_eq!(
            (
                session.seq,
                session.metadata_version,
                session.agent_state_version
            ),
            (0, 0, 0)
        );
    }
    for created in [
        Value::Null,
        json!({"session": {}}),
        json!({"session": {"id": 1}}),
    ] {
        let result = PreparedSession::prepare(&account)?.decode_response(&created);
        let error = result
            .err()
            .context("accepted a response without a session ID")?;
        assert_eq!(error.to_string(), "Happy server returned no session ID");
    }
    Ok(())
}
