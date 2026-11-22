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
//! A call or reply too large for one frame goes as [`Frame::Piece`]s of its
//! JSON, which the other side puts back together before reading it.
//!
//! Byte streams run beside the calls, many at once. The side that opens
//! one names it in the call that asks for it, by a number of its own, and
//! reads it: the other side writes it as [`Frame::Data`] and ends it with
//! [`Frame::End`]. The writer sends at most [`STREAM_WINDOW`] bytes before
//! the reader's first [`Frame::Credit`], and from then on no more than the
//! credit the reader granted in all; a reader that stops reading sends
//! [`Frame::Close`]. So a stream whose reader stalls holds up neither the
//! calls nor the other streams. A duplex stream, as a demo's, also carries
//! the opener's bytes to the other side under the same rules, each
//! direction with credit of its own: the frames about those bytes say
//! `up`, and still name the stream by the opener's number.
//! Whatever ends TLS between them can then neither pose as a side, read a
//! frame, such as a pairing code the hub issues, nor change, add, drop or
//! replay one unnoticed; only a refusal the hub sends before it can seal
//! comes as it is, and the member takes no such refusal as final.
use serde::{Deserialize, Serialize};
use shoal_ids::{
    member_ids::MemberId,
    move_tickets::MoveTicket,
    space_ids::{SpaceId, SpaceNumber},
};
use shoal_states::states;

use crate::time::UnixTime;

use super::{
    ExchangeKey, Mac, MemberKind, Nonce, Overview, PublicKey, Signature,
    availability::{Classified, Description},
    directory::{Directory, Report},
    numbering::Numbering,
    pairing::Pairing,
    routing::{Answered, Delivery, Expired, Forwarded, Queued, Told},
    sharing::{Copies, Edits},
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

/// The bytes a stream's writer may send before the reader grants credit.
pub const STREAM_WINDOW: u64 = 256 << 10;

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
    /// How the member follows the hub's version, which the hub's alert
    /// about a refused member goes by.
    pub updates: Updates,
    pub intent: Intent,
}

states! {
    /// How a member comes to run the hub's version.
    Updates {
        /// It installs the hub's version itself.
        Follows => "follows",
        /// Homebrew installed it, which the user upgrades.
        Homebrew => "homebrew",
        /// The user installs the hub's version with `shoal update`.
        Manual => "manual",
    }
}

impl Hello {
    /// The fields of what the member claims, in the order the transcript
    /// signs them: its version, installation, how it updates, nonce and
    /// exchange key, then
    /// its intent, a joiner's kind, name and key, or the member it
    /// connects as.
    fn claim(&self) -> Vec<Vec<u8>> {
        let mut fields = vec![
            self.version.as_bytes().to_vec(),
            self.instance.as_bytes().to_vec(),
            self.updates.as_str().as_bytes().to_vec(),
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
    /// A piece of a call or reply too large for one frame: the next run of
    /// the message `id`'s JSON, which the pieces after it continue while
    /// `more`.
    Piece {
        id: u64,
        data: String,
        more: bool,
    },
    /// The next bytes of the stream `stream`, in unpadded base64url, for
    /// the side that opened it, or `up` from it, no more than its credit
    /// allows.
    Data {
        stream: u64,
        data: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up: bool,
    },
    /// The end of the stream `stream`, or of the bytes its opener sends
    /// `up`, with why it failed, if it did.
    End {
        stream: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up: bool,
    },
    /// The side that opened the stream `stream` takes `bytes` more of it,
    /// or the other side of what its opener sends `up`.
    Credit {
        stream: u64,
        bytes: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up: bool,
    },
    /// The side that opened the stream `stream` stopped reading it, or the
    /// other side reading what its opener sends `up`.
    Close {
        stream: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up: bool,
    },
    Ping,
    Pong,
}

/// Why the hub refused a link, and what the member's user reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub reason: RefusalReason,
    pub message: String,
    /// The hub's own version, with a refusal of another one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<HubVersion>,
}

/// The hub's Shoal version, signed with its key over the member's nonce, so
/// a member that checks it with the key it paired with follows it although
/// the refusal that carries it comes before the link is sealed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubVersion {
    pub version: String,
    /// The newest state schema the hub's release reads, so a member whose
    /// state a newer release migrated past it never installs it.
    pub schema: i64,
    pub key: PublicKey,
    pub signature: Signature,
}

impl HubVersion {
    /// What the hub signs: its role, then `version`, `schema` and the
    /// `nonce` of the member's hello, so the signature serves that hello
    /// alone.
    pub fn signed(version: &str, schema: i64, nonce: &Nonce) -> Vec<u8> {
        let mut bytes = b"shoal hub link: version\n".to_vec();
        let schema = schema.to_be_bytes();
        for field in [version.as_bytes(), &schema, nonce.as_bytes()] {
            bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
            bytes.extend_from_slice(field);
        }
        bytes
    }
}

states! {
    RefusalReason {
        /// The member runs another Shoal version.
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
/// sends one, except a request it forwards, which names who it vouches for.
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
    /// Install the latest release on the hub. Answered with
    /// [`Reply::Updated`].
    Update,
    /// A machine's report of its spaces, with every number it holds
    /// without a space, as for a removed space's issue proposals, which no
    /// other space may take, and its capacity, which the hub places new
    /// spaces by, once it measured it. Answered with [`Reply::Reported`]
    /// and the numbers the hub keeps for the machine.
    Report {
        report: Report,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reserved: Vec<SpaceNumber>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capacity: Option<shoal_schedule::placement::snapshot::Capacity>,
    },
    /// What a machine says of itself, when its link opens and when its
    /// setting changes. Answered with [`Reply::Availability`].
    Describe { description: Description },
    /// The hub's call to a machine whose availability changed. Answered
    /// with [`Reply::Delivered`].
    Availability { availability: Classified },
    /// The latest package of the space `space`, which an intermittent
    /// machine built at `built_at` and writes up the duplex stream `stream`
    /// it opened, for the hub to keep in place of the one before. The hub
    /// ends its side of the stream once it kept the package whole, with an
    /// error when it did not. Answered with [`Reply::Streaming`].
    Keep {
        space: SpaceId,
        built_at: UnixTime,
        stream: u64,
    },
    /// The package the hub keeps of the space `space`, whose machine is
    /// offline, which the caller would take the space over from; none
    /// without one, or while that machine is connected. Answered with
    /// [`Reply::Kept`].
    Kept { space: SpaceId },
    /// The bytes of the package [`Call::Kept`] names, which the hub writes
    /// on the stream `stream` the caller opened while the space's machine
    /// stays offline. Answered with [`Reply::Streaming`].
    Fetch { space: SpaceId, stream: u64 },
    /// The hub's call to a machine whose link opened, naming the spaces
    /// other machines took over from it meanwhile, which it ends here.
    /// Answered with [`Reply::Retired`].
    Retire { spaces: Vec<SpaceId> },
    /// The final package of the space `space`, which a machine shutting
    /// down built at `built_at` once its work stopped and writes up the
    /// duplex stream `stream` it opened, for the hub to keep and continue
    /// where its placement puts `start`, the space's agent as a start
    /// would place it, among the other connected machines, the hub's own
    /// as the caller's. The hub writes that placement on its side, as an
    /// optional [`Decision`](super::placement::Decision) in JSON, none
    /// where no machine can take the space, then ends it, with an error
    /// once it did not keep the package whole. Answered with
    /// [`Reply::Streaming`].
    HandOver {
        space: SpaceId,
        built_at: UnixTime,
        stream: u64,
        start: shoal_schedule::placement::snapshot::Request,
    },
    /// A machine's word that the shutdown it handed its spaces over for was
    /// cancelled, so it stays: the hub has it retire the spaces taken over
    /// from it now, and those claimed later as each is. Answered with
    /// [`Reply::Delivered`].
    Stay,
    /// The hub's call to the machine its placement chose for the space
    /// `space`, which the machine named `from` handed over as it shut
    /// down: it continues the space from the package the hub keeps of it.
    /// Answered with [`Reply::Delivered`] once the space continues there.
    Continue { space: SpaceId, from: String },
    /// The directory of every machine's spaces. Answered with
    /// [`Reply::Directory`].
    Directory,
    /// The machine that owns the space `space`, if any, and none while the
    /// caller claims it. Answered with [`Reply::Owner`].
    Owner { space: SpaceId },
    /// Where a start of `request`, made on the calling machine, goes among
    /// the hub's machines; the hub takes the caller's machine as its own.
    /// Answered with [`Reply::Placed`], none where no machine can take it.
    Place {
        request: shoal_schedule::placement::snapshot::Request,
    },
    /// A machine's claim of the space `space`, which it imports as another
    /// machine moves it here: its report of the space takes the ID over.
    /// One that carries the `ticket` of a move through the hub is refused
    /// once that move's source settled it unclaimed, and one that takes the
    /// space over, as `takeover` says, from the package the hub keeps of it
    /// is refused once the space's machine is connected again. Answered
    /// with [`Reply::Claimed`] and the number the space keeps, if the hub
    /// lists it.
    Claim {
        space: SpaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ticket: Option<MoveTicket>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        takeover: bool,
    },
    /// The source's settling of its move `ticket` of the space `space` to
    /// the machine `machine`, once it cannot learn from that machine
    /// whether the move arrived. Answered with [`Reply::Settled`]: whether
    /// that machine claimed the space or owns it; when it did not, its
    /// claims that carry the ticket are refused from then on.
    Settle {
        space: SpaceId,
        machine: MemberId,
        ticket: MoveTicket,
    },
    /// A request for a space the machine `machine` owns, which the hub
    /// passes to it, or answers itself for its own machine; a device's goes
    /// as the user's, whatever it vouches. Answered with
    /// [`Reply::Answered`], or while that machine is offline with
    /// [`Reply::Queued`] for a request that waits for it.
    Forward {
        machine: MemberId,
        request: Forwarded,
        delivery: Delivery,
    },
    /// The hub's call to the machine that owns a forwarded request's space.
    /// Answered with [`Reply::Answered`].
    Serve { request: Forwarded },
    /// A request for a space the machine `machine` owns, whose answer that
    /// machine's daemon writes as it goes, as following a session's does,
    /// on the stream `stream` the caller opened; the hub passes it to that
    /// machine, or answers it itself for its own, and fails it at once
    /// while that machine is offline. A device's goes as the user's.
    /// Answered with [`Reply::Streaming`].
    Stream {
        machine: MemberId,
        request: Forwarded,
        stream: u64,
    },
    /// The hub's call to the machine that owns a streamed request's space,
    /// which writes the answer on the stream `stream` the hub opened.
    /// Answered with [`Reply::Streaming`].
    ServeStream { request: Forwarded, stream: u64 },
    /// An event for the space `told.above` and those above it, which the
    /// hub passes to its machine, queuing it while that machine is
    /// offline. Answered with [`Reply::Delivered`] or [`Reply::Queued`].
    Tell { told: Told },
    /// The hub's call to a machine whose queued request or event expired.
    /// Answered with [`Reply::Delivered`].
    Expired { expired: Expired },
    /// A request of an app's entries, which the hub answers from its own
    /// app store as it answers its own callers', a device's own entries
    /// being the calling member's, by writing the frames its socket would
    /// answer with on the stream `stream` the caller opened. A write
    /// carries its bytes in `data`, in unpadded base64url, and is answered
    /// on the stream with the entry alone; one without them is only
    /// checked, answered with `ok` once the hub would take it. Answered
    /// with [`Reply::Streaming`].
    App {
        method: crate::protocol::AppMethod,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        stream: u64,
    },
    /// A machine's `edits` of one kind of shared item, which the hub keeps
    /// as its copies where they change them and sends every other machine.
    /// Answered with [`Reply::Shared`]: the hub's copy of every item of
    /// that kind.
    Share { edits: Edits },
    /// The hub's call to a machine with the `copies` of one kind that
    /// changed since the hub last answered it. Answered with
    /// [`Reply::Delivered`].
    Shared { copies: Copies },
}

/// The answer to a [`Call`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Pairing(Pairing),
    Members(Overview),
    Revoked(super::Member),
    Left,
    Updated(super::HubUpdate),
    Reported(Numbering),
    Availability(Classified),
    Directory(Directory),
    Owner(Option<super::Member>),
    Placed(Option<super::placement::Decision>),
    Claimed(Option<SpaceNumber>),
    Settled(bool),
    Kept(Option<Box<super::packages::KeptPackage>>),
    /// The spaces the machine no longer has, as it ended them.
    Retired(Vec<SpaceId>),
    Answered(Answered),
    /// The answer goes on the stream the call named.
    Streaming,
    Queued(Queued),
    Delivered,
    Shared(Copies),
    Failed {
        message: String,
    },
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
