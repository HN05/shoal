//! The pairing codes the hub issues: each admits one join, within
//! [`LIFETIME`], and lives only in the daemon's memory, so a restart
//! forgets the codes it issued.
use std::{collections::HashMap, sync::Mutex, time::Duration};

use anyhow::Result;
use shoal_protocol::hub::pairing::{Pairing, PairingCode};
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

    /// Whether `code` was issued and has not expired, which it then no
    /// longer is.
    pub(super) fn take(&self, code: &str) -> bool {
        let expires = self.0.lock().unwrap().remove(code);
        expires.is_some_and(|expires| expires > Instant::now())
    }
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
