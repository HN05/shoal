//! The packages the hub keeps of an intermittent machine's spaces, from
//! which another machine takes a space over while its own is gone, and
//! where a machine shutting down hands its spaces over to.
use serde::{Deserialize, Serialize};

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
    /// It stays on its machine, for this reason.
    Stayed(String),
}
