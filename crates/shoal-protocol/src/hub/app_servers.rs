//! The hub forwards each HTTP request under a path prefix of
//! `[machine.hub] forward` to the local address it names, such as an app's
//! own server, once the request proves it comes from a paired member: it
//! names the member and carries the time it was signed at, a nonce no
//! earlier request used, and the member's signature of [`signed`] over
//! them, its method and its target, in the headers below. The hub keeps
//! the member's header, whose member it verified, and drops the others
//! before it forwards the request. The signature covers no body.
use std::{fmt, str::FromStr};

use anyhow::{Result, ensure};
use shoal_ids::member_ids::MemberId;
use shoal_states::text_key;

use super::Nonce;

/// The member a request comes from, by its ID at the hub.
pub const MEMBER: &str = "shoal-member";
/// When the member signed the request, in seconds since the Unix epoch.
pub const TIME: &str = "shoal-time";
/// The request's [`Nonce`].
pub const NONCE: &str = "shoal-nonce";
/// The member's [`super::Signature`] of [`signed`].
pub const SIGNATURE: &str = "shoal-signature";

/// How far a request's time may be from the hub's clock.
pub const SKEW: std::time::Duration = std::time::Duration::from_secs(300);

/// What a member signs to send the request `method` to `target`, its path
/// and query as the request line names them, at `time`.
pub fn signed(member: &MemberId, time: u64, nonce: &Nonce, method: &str, target: &str) -> Vec<u8> {
    let mut bytes = b"shoal hub forward\n".to_vec();
    let time = time.to_string();
    for field in [
        member.as_str().as_bytes(),
        time.as_bytes(),
        nonce.as_bytes(),
        method.as_bytes(),
        target.as_bytes(),
    ] {
        bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    bytes
}

/// The start of the paths the hub forwards to one address, such as
/// `/quay/`: it starts and ends with `/`, and holds no whitespace, query or
/// fragment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathPrefix(String);

impl PathPrefix {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the request path `path` is under it.
    pub fn covers(&self, path: &str) -> bool {
        path.starts_with(&self.0)
    }
}

impl FromStr for PathPrefix {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        ensure!(
            text.starts_with('/') && text.ends_with('/'),
            "a forwarded path prefix starts and ends with /, as /quay/ does, not {text:?}"
        );
        ensure!(
            !text.contains(|c: char| c.is_whitespace() || c.is_control() || c == '?' || c == '#'),
            "a forwarded path prefix holds no whitespace, query or fragment: {text:?}"
        );
        Ok(Self(text.to_owned()))
    }
}

impl fmt::Display for PathPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

text_key!(PathPrefix);
