//! A waiting entry's rank, with the work it took each part from, which
//! `shoal queue` shows.
use serde::{Deserialize, Serialize};

use crate::priority::Priority;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueRank {
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority_from: Option<InheritedRank>,
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_from: Option<InheritedRank>,
    /// The areas a new workspace's launch shares with its repository's
    /// other workspaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_areas: Vec<SharedArea>,
}

/// An area a launch shares. Each of `spaces` is a space's number, or its
/// name when it has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedArea {
    pub area: String,
    pub spaces: Vec<String>,
}

/// How close an entry's work is to finished. `docs/reference/queue.md` defines
/// each tier. The variants keep their order: a later tier starts first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    New,
    Started,
    Finishing,
}

/// The work an entry took part of its rank from. `from` is its space's
/// number, or a queued launch's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InheritedRank {
    pub from: String,
    pub dependency: Dependency,
}

/// Why work waits on an entry's, so that the entry takes its rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dependency {
    /// A blocker of any kind: one the launch names, one its issue's forge
    /// records, or a progress blocker.
    Blocked,
    /// The waiting work is the parent workspace of the entry's space.
    Parent,
    /// The entry's space holds a thread the waiting work opened.
    Thread,
    /// The waiting work is a workspace stacked on the entry's.
    Stacked,
    /// The waiting work is queued for a lease the entry's space holds.
    Lease,
    /// The entry is a CI job or review for the waiting work.
    Serves,
}
