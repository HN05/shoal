//! What a lookup of the forge items and runs that block launches and spaces
//! found, by blocker.
use std::collections::HashMap;

use shoal_protocol::state::BlockerKind;

/// A blocker by its kind and target: a space's ID or an item's URL.
pub type Target = (BlockerKind, String);

/// What a lookup of a blocker found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// It cleared, as this says, such as `run <url> passed on main at <commit>`.
    Cleared(String),
    Open,
    /// The lookup failed, which keeps it.
    Failed(String),
}

/// What the forge says of each blocker it was asked about.
pub type Lookups = HashMap<Target, Lookup>;

/// Whether a waiting launch's blockers let it start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// It may start, on the branch of a done blocker that is not merged.
    Ready { base: Option<String> },
    /// What still blocks it, by name or URL.
    Waiting(Vec<String>),
}
