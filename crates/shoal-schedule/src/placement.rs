//! The machine a hub places a new space on. `docs/design/hub.md` says what
//! placement decides and what it leaves to each machine's queue.
use serde::{Deserialize, Serialize};
use shoal_ids::member_ids::MemberId;
use shoal_states::states;

mod balanced;
pub mod snapshot;
pub mod speeds;

pub use balanced::Balanced;
use snapshot::{Lifetime, Snapshot};

states! {
    /// `[machine.hub] placement`: the policy that places a start which
    /// names no machine.
    #[derive(Default)]
    Policy {
        Local => "local",
        #[default]
        Balanced => "balanced",
        External => "external",
    }
}

pub trait Placer {
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

    /// Where `machine` ranks, 0 first, after every ranked machine where it
    /// ranks nowhere.
    pub fn rank_of(&self, machine: &MemberId) -> usize {
        (self.ranked.iter())
            .position(|placed| &placed.machine == machine)
            .unwrap_or(self.ranked.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placed {
    pub machine: MemberId,
    #[serde(flatten)]
    pub reason: Reason,
}

/// Why a machine comes where it does in a placement: under `balanced`, the
/// first of its stages that prefers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Reason {
    /// The policy keeps every start on the caller's machine.
    Local,
    /// An `intermittent` machine with room for disposable work, which
    /// leaves the `always_on` machines' room to lasting work.
    Intermittent,
    /// The caller's machine, beside the start's parent and unpushed work,
    /// which has room for the start.
    CallerHasRoom,
    /// A machine that does the start's repository's work at `speed`, fast
    /// enough to lead for heavy work.
    Faster { speed: speeds::Speed },
    /// A machine that does the start's repository's work at `speed`, slow
    /// enough to lead for light work, which mostly waits on its model.
    Slower { speed: speeds::Speed },
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
    /// The caller's machine, once no machine work of `lifetime` may go to
    /// has room: an `always_on` one for lasting work, any for disposable.
    Caller { lifetime: Lifetime },
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
        /// It cannot launch the start's agent in the start's repository.
        NoAgent => "no_agent",
        /// It has not reported its repositories and agents since its link
        /// opened.
        Undescribed => "undescribed",
        /// Its user walked away, and it admits nothing new.
        Away => "away",
        /// Its user paused it, and it admits nothing new.
        Paused => "paused",
    }
}

/// The `local` policy: every start stays on the caller's machine, unless
/// the snapshot lists that machine as unable to take it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Local;

impl Placer for Local {
    fn place(&self, snapshot: &Snapshot) -> Placement {
        let caller = &snapshot.request.caller;
        let listed = (snapshot.machines.iter()).find(|machine| &machine.id == caller);
        if let Some(why) = listed.and_then(|machine| snapshot.request.unfit(machine)) {
            return Placement {
                ranked: Vec::new(),
                skipped: vec![Skipped {
                    machine: caller.clone(),
                    why,
                }],
            };
        }
        Placement {
            ranked: vec![Placed {
                machine: caller.clone(),
                reason: Reason::Local,
            }],
            skipped: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
