//! Where a new space starts among the machines joined to a hub: the
//! policy that ranks the connected machines from a snapshot the hub builds
//! of the start and each machine's latest report, each with the reason it
//! comes where it does, and names the machines it leaves out and why.
//! Nothing here reads the hub's or a daemon's state; the machine a policy
//! puts first creates the space, and its own queue and admission then
//! decide when its agent starts.
use serde::{Deserialize, Serialize};
use shoal_ids::member_ids::MemberId;
use shoal_states::states;

mod balanced;
pub mod snapshot;

pub use balanced::Balanced;
use snapshot::Snapshot;

states! {
    /// `[machine.hub] placement`: the policy that places a start which
    /// names no machine.
    #[derive(Default)]
    Policy {
        /// The caller's machine, always.
        Local => "local",
        /// Lasting work on `always_on` machines with room, else the
        /// caller's machine.
        #[default]
        Balanced => "balanced",
        /// The app registered as the placer, else `balanced`.
        External => "external",
    }
}

/// An order of the machines a start may go to, read from a snapshot alone.
pub trait Placer {
    /// Where the start of `snapshot` goes: the machines in order of
    /// preference, and those it leaves out.
    fn place(&self, snapshot: &Snapshot) -> Placement;
}

/// Where a start goes: the machines in order of preference, the first
/// first, and the connected machines it cannot go to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub ranked: Vec<Placed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<Skipped>,
}

impl Placement {
    /// The machine the start goes to; none leaves it on the caller's.
    pub fn chosen(&self) -> Option<&Placed> {
        self.ranked.first()
    }
}

/// A machine a start may go to, with why it comes where it does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placed {
    pub machine: MemberId,
    #[serde(flatten)]
    pub reason: Reason,
}

/// Why a machine comes where it does in a placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Reason {
    /// The policy keeps every start on the caller's machine.
    Local,
    /// An `always_on` machine whose admission lets a heavy agent in now,
    /// with `memory_free` bytes spare, none while it reads none.
    HeavySlot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        memory_free: Option<u64>,
    },
    /// An `always_on` machine that runs no heavy agents, whose memory a
    /// light agent takes from none.
    NoHeavyAgents,
    /// An `always_on` machine where no heavy work waits, with
    /// `memory_free` bytes spare, none while it reads none.
    NoHeavyWaiting {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        memory_free: Option<u64>,
    },
    /// An `always_on` machine where `waiting` heavy entries wait.
    HeavyWaiting { waiting: u32 },
    /// The caller's machine, beside the start's parent and unpushed work,
    /// once no `always_on` machine has room.
    Caller,
}

/// A connected machine a start cannot go to, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub machine: MemberId,
    pub why: SkippedBy,
}

states! {
    /// Why a start cannot go to a machine.
    SkippedBy {
        /// It has not registered the start's repository.
        NoRepository => "no_repository",
        /// It has not installed the start's agent tool.
        NoAgent => "no_agent",
        /// Its user walked away, and it admits nothing new.
        Away => "away",
    }
}

/// The `local` policy: every start stays on the caller's machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct Local;

impl Placer for Local {
    fn place(&self, snapshot: &Snapshot) -> Placement {
        Placement {
            ranked: vec![Placed {
                machine: snapshot.request.caller.clone(),
                reason: Reason::Local,
            }],
            skipped: Vec::new(),
        }
    }
}
