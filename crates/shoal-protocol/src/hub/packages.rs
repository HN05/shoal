//! The packages the hub keeps of an intermittent machine's spaces, from
//! which another machine takes a space over while its own is gone.
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
