//! The packages the hub keeps of an intermittent machine's spaces, from
//! which another machine takes a space over while its own is gone, and
//! where a machine shutting down, or whose user walked away, hands its
//! spaces over to.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::SpaceId;
use shoal_states::states;

use crate::{
    model::{RemovedSpace, Space},
    time::UnixTime,
};

/// The package the hub keeps of a space whose machine is offline: the
/// space as the package records it, and when its machine built it, so
/// what changed after is not in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeptPackage {
    pub record: RemovedSpace,
    pub built_at: UnixTime,
}

/// Why a machine hands its spaces over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leaving {
    /// It goes away: it shuts down or sleeps, or its user walked away, so
    /// the hub's own machine, whose daemon the hub needs to place them,
    /// hands nothing over.
    Away,
    /// Its user paused it, and its daemon keeps running, so the hub's own
    /// machine hands over too.
    Paused,
}

/// The spaces a machine hands over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handing {
    /// Every space but these, which another hand-over sent or sends, with
    /// the chats in their worktrees, as it shuts down.
    AllBut(HashSet<SpaceId>),
    /// These spaces, which have something to do, as its user walked away,
    /// with the chats in their worktrees, which follow them.
    Only(HashSet<SpaceId>),
}

impl Handing {
    /// Whether the hand-over takes `space`.
    pub fn takes(&self, space: &Space) -> bool {
        match self {
            Self::AllBut(gone) => !Self::holds(gone, space),
            Self::Only(spaces) => Self::holds(spaces, space),
        }
    }

    /// The spaces it names to send, none for one that sends every space
    /// but those it names.
    pub fn named(&self) -> HashSet<SpaceId> {
        match self {
            Self::Only(spaces) => spaces.clone(),
            Self::AllBut(_) => HashSet::new(),
        }
    }

    /// Whether `spaces` holds `space`, or the workspace whose worktree the
    /// chat `space` runs in.
    fn holds(spaces: &HashSet<SpaceId>, space: &Space) -> bool {
        spaces.contains(&space.id) || space.within().is_some_and(|w| spaces.contains(w))
    }
}

/// Where a space a machine hands over goes.
#[derive(Debug)]
pub enum Handed {
    /// The hub keeps its package and continues it on the machine the
    /// decision names.
    Went(super::placement::Decision),
    /// It may go to the machine the decision names: its machine left it
    /// to the hub, whose acknowledgement did not come in time, so it keeps
    /// no restore there either.
    Unconfirmed(super::placement::Decision),
    /// It stays on its machine, for this reason.
    Stayed(String),
}

impl Handed {
    /// Whether its machine left it to the hub, so it keeps no restore
    /// there, whether or not the hub confirmed it.
    pub fn left(&self) -> bool {
        matches!(self, Self::Went(_) | Self::Unconfirmed(_))
    }
}

states! {
    /// Why the hub continues a space on another machine than its own.
    Departure {
        /// Its machine handed it over, as it shut down or its user walked
        /// away.
        HandedOver => "handed_over",
        /// Its machine slept with work to do, long enough for the hub to
        /// take it over.
        Slept => "slept",
    }
}

#[cfg(test)]
mod tests;
