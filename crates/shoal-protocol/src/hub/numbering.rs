//! The one sequence of space numbers across a hub's machines.
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};

/// The numbers the hub keeps for a machine, which it answers each of the
/// machine's reports with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Numbering {
    /// The numbers the machine gives its new spaces, smallest first.
    pub spares: Vec<SpaceNumber>,
    /// The machine's spaces whose number another machine's space held
    /// first, each with the number it takes instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub renumbered: Vec<Renumbering>,
}

/// The number the space `space` takes instead of one another machine's
/// space held first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renumbering {
    pub space: SpaceId,
    pub number: SpaceNumber,
}
