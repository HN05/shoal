use super::*;
use crypto_secretbox::aead::Nonce;

#[test]
fn legacy_bundles_are_nonce_then_secretbox() {
    let key = random_key();
    let bundle = encrypt(&key, Variant::Legacy, br#"{"role":"user"}"#).unwrap();
    assert_eq!(bundle.len(), 24 + 15 + 16);
    let cipher = XSalsa20Poly1305::new_from_slice(&key).unwrap();
    let nonce = Nonce::<XSalsa20Poly1305>::from_slice(&bundle[..24]);
    assert_eq!(
        cipher.decrypt(nonce, &bundle[24..]).unwrap(),
        br#"{"role":"user"}"#
    );
    assert!(encrypt(&key[..31], Variant::Legacy, b"x").is_err());
}

#[test]
fn data_key_bundles_are_versioned_aes_gcm() {
    let key = random_key();
    let bundle = encrypt(&key, Variant::DataKey, b"hello").unwrap();
    assert_eq!(bundle.len(), 1 + 12 + 5 + 16);
    assert_eq!(bundle[0], 0);
    let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
    let nonce = <&aes_gcm::Nonce<_>>::try_from(&bundle[1..13]).unwrap();
    assert_eq!(cipher.decrypt(nonce, &bundle[13..]).unwrap(), b"hello");
}

#[test]
fn sealed_keys_open_with_the_account_secret_key() {
    let account = SecretKey::generate(&mut OsRng);
    let session_key = random_key();
    let bundle = seal_for_account(&session_key, account.public_key().as_bytes()).unwrap();
    assert_eq!(bundle.len(), 1 + 32 + 24 + 32 + 16);
    assert_eq!(bundle[0], 0);
    let ephemeral: [u8; 32] = bundle[1..33].try_into().unwrap();
    let nonce = Nonce::<SalsaBox>::from_slice(&bundle[33..57]);
    let opened = SalsaBox::new(&PublicKey::from(ephemeral), &account)
        .decrypt(nonce, &bundle[57..])
        .unwrap();
    assert_eq!(opened, session_key);
    assert!(seal_for_account(&session_key, &[0u8; 31]).is_err());
}
