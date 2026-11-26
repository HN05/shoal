//! The key pairs links prove: the hub's own, and one per membership. Both
//! stay in the store, never leaving it.
use anyhow::{Context as _, Result, anyhow};
use ring::{
    rand::{SecureRandom as _, SystemRandom},
    signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey},
};
use rusqlite::OptionalExtension as _;
use shoal_protocol::hub::{Nonce, PublicKey, Signature};

/// A key pair as PKCS#8, which the store keeps; its `Debug` leaves the key
/// out.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyPair(Vec<u8>);

impl KeyPair {
    pub fn generate() -> Result<Self> {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|error| anyhow!("could not make a key pair: {error}"))?;
        Ok(Self(pkcs8.as_ref().to_vec()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for KeyPair {
    fn from(pkcs8: Vec<u8>) -> Self {
        Self(pkcs8)
    }
}

impl std::fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyPair(..)")
    }
}

/// A key pair and its public half as members and the hub exchange it.
pub struct Identity {
    pair: Ed25519KeyPair,
    pub public: PublicKey,
}

impl Identity {
    pub fn sign(&self, bytes: &[u8]) -> Signature {
        Signature::try_from(self.pair.sign(bytes).as_ref()).expect("Ed25519 signs 64 bytes")
    }

    pub fn new(key: &KeyPair) -> Result<Self> {
        let pair = Ed25519KeyPair::from_pkcs8(key.as_bytes())
            .map_err(|error| anyhow!("unreadable hub key pair: {error}"))?;
        let public = PublicKey::try_from(pair.public_key().as_ref())?;
        Ok(Self { pair, public })
    }
}

pub fn verify(key: &PublicKey, bytes: &[u8], signature: &Signature) -> bool {
    UnparsedPublicKey::new(&ED25519, key.as_bytes())
        .verify(bytes, signature.as_bytes())
        .is_ok()
}

/// New random bytes for the other side of a link to sign.
pub fn nonce() -> Nonce {
    Nonce::try_from(random::<32>().as_slice()).expect("32 bytes")
}

/// `N` random bytes from the system's generator.
pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("the system's random generator fails only on unsupported systems");
    bytes
}

/// The hub's key pair the store keeps, after keeping a new one when it has
/// none.
pub fn stored_or_new_key(tx: &rusqlite::Transaction<'_>) -> Result<KeyPair> {
    let stored = tx
        .query_row(
            "SELECT private_key FROM hub_identity WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(stored) = stored {
        return Ok(KeyPair(stored));
    }
    let key = KeyPair::generate()?;
    tx.execute(
        "INSERT INTO hub_identity(id, private_key) VALUES (1, ?1)",
        [key.as_bytes()],
    )
    .context("keep the hub key pair")?;
    Ok(key)
}
