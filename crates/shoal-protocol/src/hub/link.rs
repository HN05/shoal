//! The link between a member and the hub, over the hub's listener or over a
//! command relaying the hub daemon's socket. The member opens it with a
//! [`Hello`], as the first message over the listener's WebSocket or as the
//! method of the first request on the socket; then each side sends
//! [`Frame`]s, one per WebSocket message or line. The hub answers a hello
//! with a [`Frame::Challenge`] or a [`Frame::Refused`]; once the member's
//! [`Frame::Proof`] checks, a [`Frame::Welcome`], after which calls go both
//! ways.
use serde::{Deserialize, Serialize};
use shoal_ids::member_ids::MemberId;
use shoal_states::states;

use super::{MemberKind, Nonce, Overview, PublicKey, Signature, pairing::Pairing};

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
    /// The installation the member runs as, which tells a reconnection of
    /// it from another one holding a copy of its credential, such as a
    /// cloned state directory.
    pub instance: String,
    pub intent: Intent,
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
/// listener it brings a pairing code; through the daemon's socket, whose
/// callers act as the user already, none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Joining {
    pub kind: MemberKind,
    pub name: String,
    pub key: PublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// One message after the hello.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Frame {
    /// The hub's key, its signature of the member's nonce, and the nonce the
    /// member signs.
    Challenge {
        key: PublicKey,
        nonce: Nonce,
        signature: Signature,
    },
    /// The member's signature of the hub's nonce.
    Proof {
        signature: Signature,
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

/// The side of a link that signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signer {
    Hub,
    Member,
}

/// What `signer` signs to prove its key: its role, the nonce the other side
/// sent, then its own, so that a signature serves only that side of that
/// link.
pub fn signed(signer: Signer, theirs: &Nonce, ours: &Nonce) -> Vec<u8> {
    let role: &[u8] = match signer {
        Signer::Hub => b"shoal hub link: hub\n",
        Signer::Member => b"shoal hub link: member\n",
    };
    [role, theirs.as_bytes(), ours.as_bytes()].concat()
}
