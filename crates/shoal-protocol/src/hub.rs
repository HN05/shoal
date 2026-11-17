//! The hub: the daemon that takes the hub role, the machines and devices
//! paired with it, and the link each keeps to it. Every member and the hub
//! hold a key pair of their own: pairing exchanges the public keys, and each
//! link opens with both sides signing what the other sent, so a proxy or
//! tunnel between them can pose as neither.
use std::{fmt, net::SocketAddr, str::FromStr};

use anyhow::{Context as _, Result, bail, ensure};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use shoal_ids::member_ids::MemberId;
use shoal_states::{states, text_key};

use crate::time::UnixTime;

pub mod link;
pub mod pairing;

states! {
    /// What a credential belongs to, which the joiner states and the hub
    /// records on it.
    MemberKind {
        /// A daemon, which reports its own spaces and carries its callers'
        /// requests.
        Machine => "machine",
        /// An app, such as Quay, which acts as the user.
        Device => "device",
    }
}

/// The address the hub advertises, `[machine.hub] url`: the `http` or
/// `https` URL its members reach its listener at, through whatever proxy or
/// tunnel is in front of it, without a query or fragment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HubUrl(String);

impl HubUrl {
    /// The URL a member reaches a listener on `address` at directly.
    pub fn of_listener(address: SocketAddr) -> Self {
        Self(format!("http://{address}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The WebSocket URL a member opens its link at.
    pub fn link(&self) -> String {
        let (scheme, rest) = self.parts();
        let scheme = if scheme == "https" { "wss" } else { "ws" };
        format!("{scheme}://{rest}{}", link::PATH)
    }

    /// Whether traffic to it crosses the network unencrypted: `http` to a
    /// host other than this machine's loopback.
    pub fn unencrypted_remote(&self) -> bool {
        let (scheme, rest) = self.parts();
        scheme == "http" && !is_loopback(host(rest))
    }

    /// The scheme and what follows `://`.
    fn parts(&self) -> (&str, &str) {
        self.0
            .split_once("://")
            .expect("a parsed hub URL has a scheme")
    }
}

/// The host of `authority_and_path`, without its port or IPv6 brackets.
fn host(authority_and_path: &str) -> &str {
    let authority = authority_and_path
        .split('/')
        .next()
        .unwrap_or(authority_and_path);
    if let Some(bracketed) = authority.strip_prefix('[') {
        return bracketed.split(']').next().unwrap_or(bracketed);
    }
    authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host)
}

/// Whether `host` names this machine's loopback interface.
fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

impl FromStr for HubUrl {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let Some((scheme, rest)) = text.split_once("://") else {
            bail!("a hub URL starts with http:// or https://, not {text}");
        };
        ensure!(
            scheme == "http" || scheme == "https",
            "a hub URL starts with http:// or https://, not {scheme}://"
        );
        ensure!(!host(rest).is_empty(), "the hub URL {text} names no host");
        ensure!(
            !rest.contains(['?', '#']) && !rest.contains(char::is_whitespace),
            "the hub URL {text} has a query, fragment or space"
        );
        Ok(Self(format!("{scheme}://{}", rest.trim_end_matches('/'))))
    }
}

impl fmt::Display for HubUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

text_key!(HubUrl);

/// Defines `$name`, `$len` bytes written as unpadded base64url text.
macro_rules! encoded {
    ($(#[$meta:meta])* $name:ident, $len:literal) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash)]
        pub struct $name([u8; $len]);

        impl $name {
            pub fn as_bytes(&self) -> &[u8] {
                &self.0
            }
        }

        impl TryFrom<&[u8]> for $name {
            type Error = anyhow::Error;

            fn try_from(bytes: &[u8]) -> Result<Self> {
                let bytes = bytes.try_into().with_context(|| {
                    format!(
                        "{} takes {} bytes, not {}",
                        stringify!($name),
                        $len,
                        bytes.len()
                    )
                })?;
                Ok(Self(bytes))
            }
        }

        impl FromStr for $name {
            type Err = anyhow::Error;

            fn from_str(text: &str) -> Result<Self> {
                let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(text)
                    .with_context(|| format!("{} is not base64url", stringify!($name)))?;
                Self::try_from(bytes.as_slice())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({self})", stringify!($name))
            }
        }

        text_key!($name);
    };
}

encoded!(
    /// An Ed25519 public key, which proves a member or the hub.
    PublicKey,
    32
);
encoded!(
    /// An Ed25519 signature.
    Signature,
    64
);
encoded!(
    /// Random bytes one side of a link sends for the other to sign.
    Nonce,
    32
);
encoded!(
    /// An ephemeral X25519 public key, which one side of a link sends for
    /// the key exchange that seals it.
    ExchangeKey,
    32
);
encoded!(
    /// An HMAC-SHA256 tag, which proves a joiner holds a pairing code.
    Mac,
    32
);
encoded!(
    /// The SHA-256 digest of a public key, which a pairing code carries for
    /// the joiner to check the hub's key against.
    Fingerprint,
    32
);

impl PublicKey {
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint(sha2::Sha256::digest(self.0).into())
    }
}

/// A machine or device paired with the hub, the hub's own machine included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: MemberId,
    pub kind: MemberKind,
    pub name: String,
    /// The Shoal version it last connected with; the hub's own for the hub.
    pub version: Option<String>,
    pub joined_at: UnixTime,
    /// When its link last carried a frame; none for one that never
    /// connected, and for the hub, which is always there.
    pub last_seen: Option<UnixTime>,
    /// Whether its link is open now, as the hub's own machine always is.
    pub connected: bool,
    /// Whether it is the hub's own machine.
    pub hub: bool,
}

/// The hub's listener: where it listens, which Shoal never encrypts, and the
/// URL it advertises.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listener {
    /// The address it is bound to; none while it cannot bind.
    pub address: Option<SocketAddr>,
    pub url: HubUrl,
    /// Why it is not listening.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What `shoal hub ls` shows: the hub's listener and its members.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Overview {
    pub listener: Listener,
    pub members: Vec<Member>,
}

/// How a member reaches the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// A WebSocket to the hub's URL.
    Url(HubUrl),
    /// The standard input and output of a command, `[machine.hub] connect`,
    /// that relays them to the hub's daemon.
    Command(Vec<String>),
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(url) => url.fmt(f),
            Self::Command(command) => f.write_str(&command.join(" ")),
        }
    }
}

/// This machine's membership of a hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    /// The ID the hub gave this machine.
    pub member: MemberId,
    pub name: String,
    /// The URL this machine paired at, or the command it joined through.
    pub through: Route,
    pub joined_at: UnixTime,
}

/// The hub role this daemon takes, which `shoal status` shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum HubRole {
    /// It is the hub.
    Hub { listener: Listener },
    /// It joined a hub, which it reaches by `route`: `[machine.hub]
    /// connect` when set, else what it joined through.
    Member {
        membership: Membership,
        route: Route,
        connected: bool,
        /// Why its link is down, or why it no longer tries to open one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<String>,
    },
}

/// What `shoal hub leave` did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Left {
    pub membership: Membership,
    /// Whether the hub revoked the credential; one that could not be
    /// reached still lists the machine until `shoal hub revoke` there.
    pub revoked: bool,
}

#[cfg(test)]
mod tests;
