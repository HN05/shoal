//! The link between a member and the hub, over the hub's listener or over a
//! command relaying the hub daemon's socket. The member opens it with a
//! [`Hello`], as the first message over the listener's WebSocket or as the
//! method of the first request on the socket; then each side sends
//! [`Frame`]s, one per WebSocket message or line. The hub answers a hello
//! with a [`Frame::Challenge`] or a [`Frame::Refused`]; once the member's
//! [`Frame::Proof`] checks, a [`Frame::Welcome`], after which calls go both
//! ways.
//!
//! Both sides sign the [`Transcript`] of the handshake: the member's hello,
//! with who it claims to be, and each side's nonce and ephemeral X25519 key. A joiner with a pairing code
//! proves it holds the code without sending it: its proof carries the
//! HMAC-SHA256, keyed with the code's text, of what it signs. From the
//! proof on, each side sends every frame [`Sealed`]: numbered from 0 in
//! each direction and encrypted with ChaCha20-Poly1305, whose 12-byte nonce
//! is 4 zero bytes and then the number, big-endian, under a key HKDF-SHA256
//! derives from the X25519 shared secret, with the member's nonce then the
//! hub's as salt and [`MEMBER_TO_HUB`] or [`HUB_TO_MEMBER`] as info.
//! Whatever ends TLS between them can then neither pose as a side, read a
//! frame, such as a pairing code the hub issues, nor change, add, drop or
//! replay one unnoticed; only a refusal the hub sends before it can seal
//! comes as it is, and the member takes no such refusal as final.
use serde::{Deserialize, Serialize};
use shoal_ids::member_ids::MemberId;
use shoal_states::states;

use super::{
    ExchangeKey, Mac, MemberKind, Nonce, Overview, PublicKey, Signature, pairing::Pairing,
};

/// The info that derives the key of frames the member sends.
pub const MEMBER_TO_HUB: &[u8] = b"shoal hub link: member to hub";
/// The info that derives the key of frames the hub sends.
pub const HUB_TO_MEMBER: &[u8] = b"shoal hub link: hub to member";

/// The path of the listener's WebSocket.
pub const PATH: &str = "/hub";

/// How long a side waits without a frame from the other before it takes the
/// link for lost; each side sends [`Frame::Ping`] at a third of it.
pub const SILENCE: std::time::Duration = std::time::Duration::from_secs(90);

/// The method of a request that opens a link on the daemon's socket. The hub
/// reads the hello it carries only after checking the request's protocol, so
/// it can refuse a member of another release, whose hello it may not read.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Opening {
    HubLink(serde_json::Value),
}

/// The member's first message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    /// The member's Shoal version, which a refusal for another protocol
    /// names.
    pub version: String,
    /// What the hub signs to prove its key.
    pub nonce: Nonce,
    /// The member's ephemeral X25519 key, which seals the link.
    pub exchange: ExchangeKey,
    /// The installation the member runs as, which tells a reconnection of
    /// it from another one holding a copy of its credential, such as a
    /// cloned state directory.
    pub instance: String,
    pub intent: Intent,
}

impl Hello {
    /// The fields of what the member claims, in the order the transcript
    /// signs them: its version, installation, nonce and exchange key, then
    /// its intent, a joiner's kind, name and key, or the member it
    /// connects as.
    fn claim(&self) -> Vec<Vec<u8>> {
        let mut fields = vec![
            self.version.as_bytes().to_vec(),
            self.instance.as_bytes().to_vec(),
            self.nonce.as_bytes().to_vec(),
            self.exchange.as_bytes().to_vec(),
        ];
        match &self.intent {
            Intent::Join(Joining { kind, name, key }) => fields.extend([
                b"join".to_vec(),
                kind.as_str().as_bytes().to_vec(),
                name.as_bytes().to_vec(),
                key.as_bytes().to_vec(),
            ]),
            Intent::Connect { member } => {
                fields.extend([b"connect".to_vec(), member.as_str().as_bytes().to_vec()])
            }
        }
        fields
    }
}

/// What the member opens the link for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Pair as a new member.
    Join(Joining),
    /// Connect as `member`.
    Connect { member: MemberId },
}

/// A new member: its kind and name, and the key it holds. Over the
/// listener its proof also proves a pairing code; through the daemon's
/// socket, whose callers act as the user already, it needs none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Joining {
    pub kind: MemberKind,
    pub name: String,
    pub key: PublicKey,
}

/// One message after the hello.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Frame {
    /// The hub's key, its nonce and ephemeral key, and its signature of the
    /// transcript they complete.
    Challenge {
        key: PublicKey,
        nonce: Nonce,
        exchange: ExchangeKey,
        signature: Signature,
    },
    /// The member's signature of the transcript, and a joiner's proof of
    /// its pairing code.
    Proof {
        signature: Signature,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<Mac>,
    },
    /// The hub took the member, under the ID `member`; `hub` is the hub's
    /// own machine's.
    Welcome {
        member: MemberId,
        name: String,
        hub: MemberId,
    },
    /// The hub refused the link, which it then closes. Its spelling stays,
    /// so a member of another release reads it.
    Refused(Refusal),
    Call {
        id: u64,
        call: Call,
    },
    Reply {
        id: u64,
        reply: Reply,
    },
    Ping,
    Pong,
}

/// Why the hub refused a link, and what the member's user reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub reason: RefusalReason,
    pub message: String,
}

states! {
    RefusalReason {
        /// The member speaks another protocol version.
        Version => "version",
        /// The hub revoked the member's credential.
        Revoked => "revoked",
        /// Another link holds the credential, as from a copied state
        /// directory.
        Duplicate => "duplicate",
        /// The pairing code is unknown, used or expired, or a join over the
        /// listener has none.
        Code => "code",
        /// The member's signature does not check, or names no member.
        Proof => "proof",
        /// The daemon does not take the hub role.
        NotHub => "not_hub",
        /// The hub's daemon is about to restart, and records no new member
        /// meanwhile.
        Restarting => "restarting",
    }
}

/// A request one side sends the other once the link is open. Calls from a
/// member act as the user: a machine refuses its scoped callers before it
/// sends one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Call {
    /// A new pairing code. Answered with [`Reply::Pairing`].
    Pair,
    /// The listener and the members. Answered with [`Reply::Members`].
    Members,
    /// Revoke the member `member` names by ID or name. Answered with
    /// [`Reply::Revoked`].
    Revoke { member: String },
    /// Revoke the caller's own credential. Answered with [`Reply::Left`].
    Leave,
}

/// The answer to a [`Call`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Pairing(Pairing),
    Members(Overview),
    Revoked(super::Member),
    Left,
    Failed { message: String },
}

/// A frame after the proof: its number in its direction, and its text
/// encrypted with its tag, in unpadded base64url.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sealed {
    pub seq: u64,
    pub data: String,
}

/// The side of a link that signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signer {
    Hub,
    Member,
}

/// What both sides of a link's handshake sent, which each signs: the
/// member's hello, with everything it claims, and the hub's nonce and
/// exchange key.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub hello: Hello,
    pub hub_nonce: Nonce,
    pub hub_exchange: ExchangeKey,
}

impl Transcript {
    /// What `signer` signs to prove its key: its role, then the transcript,
    /// so that a signature serves only that side of that link, and covers
    /// who the member claims to be.
    pub fn signed(&self, signer: Signer) -> Vec<u8> {
        let role: &[u8] = match signer {
            Signer::Hub => b"shoal hub link: hub\n",
            Signer::Member => b"shoal hub link: member\n",
        };
        let mut bytes = role.to_vec();
        for field in self.hello.claim().iter().chain([
            &self.hub_nonce.as_bytes().to_vec(),
            &self.hub_exchange.as_bytes().to_vec(),
        ]) {
            bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
            bytes.extend_from_slice(field);
        }
        bytes
    }

    /// The salt of the link's keys: the member's nonce, then the hub's.
    pub fn salt(&self) -> Vec<u8> {
        [self.hello.nonce.as_bytes(), self.hub_nonce.as_bytes()].concat()
    }
}
