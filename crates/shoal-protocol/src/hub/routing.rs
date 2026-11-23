//! What one joined machine sends another through the hub: a request for a
//! space the other owns, which the owner decides as it decides its own
//! callers', and an event a space passes to the space above it there.
use serde::{Deserialize, Serialize};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};

use crate::{
    alerts::AlertKind,
    config::permissions::{PermissionKey, Permissions},
    files::SentFile,
    space_names::SpaceName,
    time::UnixTime,
};

/// Who a forwarded request comes from, as the machine that forwards it
/// vouches: each machine vouches for its own callers, and a device's
/// requests are the user's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vouched {
    /// The user: an unscoped caller of the forwarding machine, or a device.
    User,
    /// A process of a space on the forwarding machine.
    Space(VouchedSpace),
}

/// A space's process whose request its machine forwards, with what the
/// owning machine needs to decide it as that space's: the reach of its
/// scope and its effective permissions there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VouchedSpace {
    pub space: SpaceId,
    /// Whether its scope reaches every space, as a chat's without a
    /// repository does.
    pub everywhere: bool,
    /// Its effective permissions, as its machine computes them in its own
    /// repository, or for a chat without one in the global config.
    pub permissions: Permissions,
    /// The permission an approval let its command take actions of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted: Option<PermissionKey>,
    /// It only reads, as the gate's runs do.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    /// Its space's priority, the most it gives the spaces below it.
    pub priority: shoal_schedule::priority::Priority,
    /// For a chat without a repository, whose permissions resolve in the
    /// repository each action is in: its tree's modes, which the owning
    /// machine resolves in the action's repository there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chat_modes: Vec<ChainedMode>,
}

/// The mode a space's agent started in, with the values its `--permission`
/// gave, in the chain of a chat without a repository from the top of its
/// tree down to it, each narrowed by the one above.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainedMode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::config::modes::ModeName>,
    #[serde(default)]
    pub given: crate::config::permissions::PermissionsConfig,
}

/// A request a machine forwards to the machine that owns its space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Forwarded {
    pub caller: Vouched,
    /// The request's method, as its caller sent it, which the owning
    /// machine reads as its own callers' requests.
    pub method: serde_json::Value,
    /// What the request does, for its sender to read should it expire in
    /// the hub's queue.
    pub summary: String,
    /// The space it acts on, where its machine was chosen by one, which a
    /// request the hub holds follows should the hub take the space over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<SpaceId>,
    /// Whether its sender hears nothing of it should it expire, or fail,
    /// in the hub's queue, as for what only keeps another machine in step
    /// with this one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unheard: bool,
}

/// How the hub delivers a forwarded request whose machine is offline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// It fails at once, naming the machine and when the hub last saw it.
    Now,
    /// It waits in the hub's queue until the machine connects again.
    Queued,
}

/// The owning machine's answer to a forwarded request: the response frames
/// its daemon wrote, which the forwarding daemon writes to its caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Answered {
    pub frames: Vec<serde_json::Value>,
}

/// A request the hub holds for an offline machine, which its caller reads
/// as the request's outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queued {
    pub machine: MemberId,
    pub name: String,
    pub last_seen: Option<UnixTime>,
    /// When the hub drops it unless the machine connects first.
    pub expires_at: UnixTime,
}

/// An event a space passes up the tree to a space on another machine:
/// what its coordinator receives, which the machine of `above` delivers to
/// the nearest space from `above` up with an agent, passing it on to the
/// next machine where the tree goes on there, or to the user at the top.
/// Without `about`, it is a message for `above` itself, such as an approval
/// request routed to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Told {
    /// The space whose coordinator receives it, or the nearest above it.
    pub above: SpaceId,
    /// The space below `above` it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<ToldAbout>,
    /// Whether the user receives it when no space above takes it.
    pub for_user: bool,
    /// What it tells, from which the receiving machine builds its alert.
    pub record: ToldRecord,
}

/// What a [`Told`] tells, which the machine that receives it turns into
/// its own alert as it would for an event of its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToldRecord {
    /// An alert of `kind` with its text and the records of the files it
    /// sent, which the sending machine keeps.
    Alert {
        kind: AlertKind,
        message: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<SentFile>,
    },
    /// A thread's post, forward or resolution of `kind`: the text the agent
    /// that takes it reads, as an agent on the thread's machine would, and
    /// the line the user reads when no agent takes it.
    Thread {
        kind: AlertKind,
        agent: String,
        user: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<SentFile>,
    },
}

impl ToldRecord {
    /// Its kind of alert.
    pub fn kind(&self) -> AlertKind {
        match self {
            Self::Alert { kind, .. } | Self::Thread { kind, .. } => *kind,
        }
    }

    /// What an agent that takes it reads, and what the user reads.
    pub fn texts(&self) -> (String, String) {
        match self {
            Self::Alert { message, .. } => (message.clone(), message.clone()),
            Self::Thread { agent, user, .. } => (agent.clone(), user.clone()),
        }
    }

    /// The records of the files it sent.
    pub fn files(&self) -> &[SentFile] {
        match self {
            Self::Alert { files, .. } | Self::Thread { files, .. } => files,
        }
    }
}

/// The space an event is about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToldAbout {
    pub space: SpaceId,
    pub name: SpaceName,
}

/// What the hub tells a machine whose queued request or event did not go
/// through: it expired, reached its machine without word that it ran, or
/// was refused there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Expired {
    /// The space whose process sent it, which hears of it, or none for the
    /// user's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<SpaceId>,
    /// The machine it was for.
    pub machine: String,
    /// What it was.
    pub summary: String,
    pub queued_at: UnixTime,
    pub outcome: HeldOutcome,
}

/// What became of a request or event the hub held for an offline machine,
/// which its sender hears of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldOutcome {
    /// It expired before the machine connected.
    Expired,
    /// It reached the machine once it connected, which never confirmed
    /// that it ran.
    Unconfirmed,
    /// The machine refused it, for `reason`.
    Refused { reason: String },
}

impl std::fmt::Display for Queued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let now = UnixTime::now();
        let seen = match self.last_seen {
            Some(seen) => format!("last seen {}", seen.local_ago(now)),
            None => "never connected".into(),
        };
        write!(
            f,
            "machine {} is offline ({seen}); the hub delivers this once it connects, unless it is still offline at {}",
            self.name,
            self.expires_at.local_minutes()
        )
    }
}

impl std::error::Error for Queued {}
