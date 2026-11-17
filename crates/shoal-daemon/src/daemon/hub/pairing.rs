//! The pairing codes the hub issues: each admits one join, within
//! [`LIFETIME`], and lives only in the daemon's memory, so a restart
//! forgets the codes it issued.
use std::{collections::HashMap, sync::Mutex, time::Duration};

use anyhow::Result;
use ring::hmac;
use shoal_protocol::hub::{
    Mac,
    pairing::{Pairing, PairingCode},
};
use tokio::time::Instant;

use super::identity::random;
use crate::{daemon::workspace::Manager, time::UnixTime};

/// How long a code admits a join.
pub(super) const LIFETIME: Duration = Duration::from_secs(10 * 60);

/// The letters a code takes: no `0`, `1`, `I` or `O`, which read alike, and
/// 32 of them, so each random byte picks one without bias.
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// The letters of a code: 100 bits.
const LENGTH: usize = 20;

/// The codes the hub issued and has yet to take, each with when it expires.
#[derive(Default)]
pub(super) struct Codes(Mutex<HashMap<String, Instant>>);

impl Codes {
    /// A new code, which [`Codes::take`] takes once within [`LIFETIME`].
    fn issue(&self) -> String {
        let code: String = random::<LENGTH>()
            .iter()
            .map(|byte| char::from(ALPHABET[usize::from(byte % 32)]))
            .collect();
        let mut codes = self.0.lock().unwrap();
        let now = Instant::now();
        codes.retain(|_, expires| *expires > now);
        codes.insert(code.clone(), now + LIFETIME);
        code
    }

    /// Whether a code that was issued and has not expired `proves` itself,
    /// which it then no longer is.
    pub(super) fn take_proven(&self, proves: impl Fn(&str) -> bool) -> bool {
        let mut codes = self.0.lock().unwrap();
        let now = Instant::now();
        codes.retain(|_, expires| *expires > now);
        let proven = codes.keys().find(|code| proves(code)).cloned();
        proven.is_some_and(|code| codes.remove(&code).is_some())
    }
}

/// What proves a joiner holds `code` without sending it: the HMAC-SHA256,
/// keyed with the code's text, of what the joiner signs, `signed`.
pub(super) fn code_proof(code: &str, signed: &[u8]) -> Mac {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, code.as_bytes()), signed);
    Mac::try_from(tag.as_ref()).expect("HMAC-SHA256 tags are 32 bytes")
}

/// Whether `proof` proves `code` for what the joiner signed, `signed`.
pub(super) fn proves_code(code: &str, signed: &[u8], proof: &Mac) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, code.as_bytes());
    hmac::verify(&key, signed, proof.as_bytes()).is_ok()
}

impl Manager {
    /// A new pairing code, with the URL the hub advertises and its key.
    pub(super) async fn issue_pairing(&self) -> Result<Pairing> {
        let url = self.hub_listener().url;
        let key = self.hub_identity().await?.public.fingerprint();
        let code = self.hub.codes.issue();
        Ok(Pairing {
            code: PairingCode { url, key, code },
            expires_at: UnixTime::now() + LIFETIME,
        })
    }
}

#[cfg(test)]
mod tests;
