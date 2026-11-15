//! A waiting entry's rank, which says why it starts where it does: its
//! priority and tier, each with the work it took them from.
use serde::{Deserialize, Serialize};

use crate::priority::Priority;

/// A waiting entry's rank: its priority and tier, each with the work it
/// took that from when that work waits on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueRank {
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority_from: Option<InheritedRank>,
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_from: Option<InheritedRank>,
    /// The areas a launch of a new workspace shares with the workspaces of
    /// its repository, which put it after launches of its tier that share
    /// fewer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_areas: Vec<SharedArea>,
}

/// An area a launch shares, with the spaces holding it: their numbers, or
/// their names without one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedArea {
    pub area: String,
    pub spaces: Vec<String>,
}

/// How close an entry's work is to finished; later tiers start first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Launches, and CI runs started without a PR.
    New,
    /// Restores of agents that have worked in their space.
    Started,
    /// Work of a space with an open PR or a ready mark, and CI jobs on open
    /// PRs and on the default branch.
    Finishing,
}

/// The work an entry took its rank from: `from` is its space's number, or a
/// queued launch's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InheritedRank {
    pub from: String,
    pub dependency: Dependency,
}

/// Why work waits on an entry's, so that the entry takes its rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dependency {
    /// It is blocked on the entry's work: an `--after` blocker, one its
    /// issue's forge records, or a progress blocker.
    Blocked,
    /// It is the parent workspace of the entry's space.
    Parent,
    /// It is a child of the entry's space and asks it a question.
    Question,
    /// It is a workspace stacked on the entry's.
    Stacked,
    /// It waits for a lease the entry's space holds.
    Lease,
    /// The entry is a CI job or review for it.
    Serves,
}
