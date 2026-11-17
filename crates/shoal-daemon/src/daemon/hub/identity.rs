//! This daemon's key pair, which proves it to the hub it joins or, as the
//! hub, to its members: made the first time a link needs it and kept in the
//! store, never leaving it.
use anyhow::{Context as _, Result, anyhow};
use ring::{
    rand::{SecureRandom as _, SystemRandom},
    signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey},
};
use rusqlite::OptionalExtension as _;
use shoal_protocol::hub::{Nonce, PublicKey, Signature};

use crate::daemon::workspace::Manager;

/// A key pair and its public half as members and the hub exchange it.
pub(crate) struct Identity {
    pair: Ed25519KeyPair,
    pub public: PublicKey,
}

impl Identity {
    pub(crate) fn sign(&self, bytes: &[u8]) -> Signature {
        Signature::try_from(self.pair.sign(bytes).as_ref()).expect("Ed25519 signs 64 bytes")
    }

    fn from_pkcs8(pkcs8: &[u8]) -> Result<Self> {
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8)
            .map_err(|error| anyhow!("unreadable hub key pair: {error}"))?;
        let public = PublicKey::try_from(pair.public_key().as_ref())?;
        Ok(Self { pair, public })
    }
}

/// Whether `signature` is `key`'s of `bytes`.
pub(crate) fn verify(key: &PublicKey, bytes: &[u8], signature: &Signature) -> bool {
    UnparsedPublicKey::new(&ED25519, key.as_bytes())
        .verify(bytes, signature.as_bytes())
        .is_ok()
}

/// New random bytes for the other side of a link to sign.
pub(crate) fn nonce() -> Nonce {
    Nonce::try_from(random::<32>().as_slice()).expect("32 bytes")
}

/// `N` random bytes from the system's generator.
pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("the system's random generator fails only on unsupported systems");
    bytes
}

impl Manager {
    /// This daemon's key pair, made and kept the first time it is asked for.
    pub(crate) async fn hub_identity(&self) -> Result<std::sync::Arc<Identity>> {
        let identity = &self.hub.identity;
        let identity = identity.get_or_try_init(|| async {
            let pkcs8 = self.store.write(stored_or_new_key).await?;
            Identity::from_pkcs8(&pkcs8).map(std::sync::Arc::new)
        });
        identity.await.cloned()
    }
}

/// The key pair the store keeps, as PKCS#8, after keeping a new one when it
/// has none.
fn stored_or_new_key(tx: &rusqlite::Transaction<'_>) -> Result<Vec<u8>> {
    let stored = tx
        .query_row(
            "SELECT private_key FROM hub_identity WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(stored) = stored {
        return Ok(stored);
    }
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|error| anyhow!("could not make a hub key pair: {error}"))?;
    tx.execute(
        "INSERT INTO hub_identity(id, private_key) VALUES (1, ?1)",
        [pkcs8.as_ref()],
    )
    .context("keep the hub key pair")?;
    Ok(pkcs8.as_ref().to_vec())
}
