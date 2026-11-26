//! What a scheduler reads, as the daemon builds it from its state.
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
};

use crate::{
    priority::Priority,
    rank::{Dependency, Tier},
    stopping::Running,
};
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_time::UnixTime;

#[derive(Debug, Default)]
pub struct Snapshot {
    pub entries: Vec<Entry>,
    /// The spaces of the entries and of the work waiting on them.
    pub spaces: HashMap<SpaceId, Space>,
    pub edges: Vec<Edge>,
    pub now: UnixTime,
    pub occupied: HashMap<RepositoryId, Occupied>,
    pub running: Running,
}

/// The areas a repository's workspaces occupy, each with the workspaces
/// holding it.
pub type Occupied = BTreeMap<String, Vec<SpaceId>>;

#[derive(Debug)]
pub struct Entry {
    pub id: QueueEntryId,
    /// What explanations call a launch that has no space yet.
    pub name: String,
    /// The priority a launch was queued with, which ranks it while it has
    /// no space or its space has none of its own.
    pub priority: Option<Priority>,
    pub work: Work,
    pub queued_at: UnixTime,
    /// Its place in the order entries arrived in.
    pub arrival: usize,
    /// When the prompt cache of a restore's agent expires, as its hooks last
    /// reported it.
    pub prompt_cache_until: Option<UnixTime>,
    pub repository: Option<RepositoryId>,
    /// The areas of the issue a launch starts on.
    pub areas: Vec<String>,
}

impl Entry {
    /// The node of an agent's entry: its space, or the launch itself.
    pub(crate) fn agent_node(&self) -> Option<Node> {
        match &self.work {
            Work::Launch { space: Some(space) } | Work::Restore { space } => {
                Some(Node::Space(space.clone()))
            }
            Work::Launch { space: None } => Some(Node::Entry(self.id.clone())),
            Work::Job { .. } | Work::Review { .. } => None,
        }
    }
}

/// What a waiting entry starts.
#[derive(Debug)]
pub enum Work {
    /// An agent's launch, in the space it opens once that is known.
    Launch { space: Option<SpaceId> },
    /// The restore of an agent that has worked in `space`.
    Restore { space: SpaceId },
    /// A CI job of its own `tier`, for `spaces`.
    Job { tier: Tier, spaces: Vec<SpaceId> },
    /// A review of `space`. A review the daemon left running has none.
    Review { space: Option<SpaceId> },
}

#[derive(Debug)]
pub struct Space {
    /// What explanations call it: its number, else its name.
    pub label: String,
    /// The priority set on it, else the highest its issue's labels map to;
    /// it has the default without one.
    pub priority: Option<Priority>,
    /// Its tier before it inherits from waiting work.
    pub tier: Tier,
    pub steps: Steps,
    pub created_at: UnixTime,
}

/// How many of a space's plan steps are done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Steps {
    pub done: usize,
    pub total: usize,
}

impl Steps {
    /// Compares the shares of steps done. A space without steps has none
    /// done.
    pub(crate) fn cmp_share(self, other: Self) -> Ordering {
        let share = |steps: Self, by: Self| steps.done * by.total.max(1);
        share(self, other).cmp(&share(other, self))
    }
}

/// Work that waits or is waited on: a space, or an entry of its own, such
/// as a queued launch that has opened no space yet or a CI job.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    Space(SpaceId),
    Entry(QueueEntryId),
}

/// `waiting` waits on `on`, so `on` ranks at least as high as what the
/// edge carries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Edge {
    pub waiting: Node,
    pub on: Node,
    pub dependency: Dependency,
    pub carries: Carries,
}

/// What of the waiting work's rank an edge passes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Carries {
    /// Its priority and tier, its own or inherited.
    Rank,
    /// Only a priority its space has of its own or was passed, as a chat
    /// passes to its children.
    SetPriority,
}
