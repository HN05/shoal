//! The encryption Happy clients apply to session records and messages, as
//! implemented by happy-cli and its `happy-agent` control-plane client: legacy
//! accounts encrypt everything with the account secret (NaCl secretbox);
//! data-key accounts give each session a random AES-256-GCM key sealed to the
//! account's NaCl box public key.
use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, AeadCore, OsRng, rand_core::RngCore},
};
use anyhow::{Context, Result, ensure};
use crypto_box::{PublicKey, SalsaBox, SecretKey};
use crypto_secretbox::XSalsa20Poly1305;

/// How a session's records and messages are encrypted; Happy's wire spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Legacy,
    DataKey,
}

impl Variant {
    pub fn name(self) -> &'static str {
        match self {
            Variant::Legacy => "legacy",
            Variant::DataKey => "dataKey",
        }
    }
}

pub fn random_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
    key
}

/// Encrypt a JSON payload the way Happy expects to decrypt it.
pub fn encrypt(key: &[u8], variant: Variant, plaintext: &[u8]) -> Result<Vec<u8>> {
    ensure!(key.len() == 32, "Happy encryption keys are 32 bytes");
    match variant {
        // nonce(24) || secretbox ciphertext
        Variant::Legacy => {
            let cipher = XSalsa20Poly1305::new_from_slice(key)?;
            let nonce = XSalsa20Poly1305::generate_nonce(&mut OsRng);
            let ciphertext = cipher
                .encrypt(&nonce, plaintext)
                .context("secretbox encryption failed")?;
            Ok([nonce.as_slice(), ciphertext.as_slice()].concat())
        }
        // version(1) || nonce(12) || ciphertext || tag(16)
        Variant::DataKey => {
            let cipher = Aes256Gcm::new_from_slice(key)?;
            let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
            let ciphertext = cipher
                .encrypt(&nonce, plaintext)
                .context("AES-GCM encryption failed")?;
            Ok([&[0u8][..], nonce.as_slice(), ciphertext.as_slice()].concat())
        }
    }
}

/// Seal a session key for the account: version(1) || ephemeral public key(32)
/// || nonce(24) || NaCl box ciphertext, the layout Happy stores as a session's
/// `dataEncryptionKey`.
pub fn seal_for_account(data: &[u8], account_public_key: &[u8]) -> Result<Vec<u8>> {
    let recipient: [u8; 32] = account_public_key
        .try_into()
        .context("Happy account public key must be 32 bytes")?;
    let ephemeral = SecretKey::generate(&mut OsRng);
    let nonce = SalsaBox::generate_nonce(&mut OsRng);
    let ciphertext = SalsaBox::new(&PublicKey::from(recipient), &ephemeral)
        .encrypt(&nonce, data)
        .context("box encryption failed")?;
    Ok([
        &[0u8][..],
        ephemeral.public_key().as_bytes().as_slice(),
        nonce.as_slice(),
        ciphertext.as_slice(),
    ]
    .concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::Nonce;

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
        let nonce = Nonce::<Aes256Gcm>::from_slice(&bundle[1..13]);
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
}
