//! The packages the hub keeps of an intermittent machine's spaces, from
//! which another machine takes a space over while its own is gone, and
//! where a machine shutting down hands its spaces over to.
use serde::{Deserialize, Serialize};
use shoal_states::states;

use crate::{model::RemovedSpace, time::UnixTime};

/// The package the hub keeps of a space whose machine is offline: the
/// space as the package records it, and when its machine built it, so
/// what changed after is not in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeptPackage {
    pub record: RemovedSpace,
    pub built_at: UnixTime,
}

/// Where a space a machine hands over as it shuts down goes.
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
        /// Its machine handed it over as it shut down.
        ShutDown => "shut_down",
        /// Its machine slept with work to do, long enough for the hub to
        /// take it over.
        Slept => "slept",
    }
}
