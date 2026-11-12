//! The rank of the queue's waiting work, which decides what starts first.
//! Each entry has a tier, how close its work is to finished, which rises one
//! level for each hour it waits. Tiers flow along the dependency graph, from
//! work that waits to the work it waits on, so that an entry ranks at least
//! as high as anything waiting on it; a CI job or review ranks with the
//! spaces it serves. Within a tier the larger share of a space's plan steps
//! done goes first, then the space started earliest, then a CI job or review
//! ahead of an agent, then arrival.
use std::{
    cmp::Ordering,
    collections::{HashMap, hash_map},
    time::Duration,
};

use crate::client::{queue::QueueEntryId, space_ids::SpaceId, time::UnixTime};
use crate::model::{Dependency, InheritedRank, QueueRank, Tier};

/// How long an entry waits to rise one tier.
const RISE_AFTER: Duration = Duration::from_secs(60 * 60);

/// What ranking reads: the waiting entries, the spaces they and the work
/// waiting on them belong to, and what waits on what.
#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    pub entries: Vec<Entry>,
    pub spaces: HashMap<SpaceId, Space>,
    pub edges: Vec<Edge>,
    pub now: UnixTime,
}

/// A waiting entry.
#[derive(Debug)]
pub(crate) struct Entry {
    pub id: QueueEntryId,
    /// What explanations call a launch that has no space yet.
    pub name: String,
    pub work: Work,
    pub queued_at: UnixTime,
    /// Its place in the order entries arrived in.
    pub arrival: usize,
}

/// What a waiting entry starts.
#[derive(Debug)]
pub(crate) enum Work {
    /// An agent's launch, in the space it opens once that is known.
    Launch { space: Option<SpaceId> },
    /// The restore of an agent that has worked in `space`.
    Restore { space: SpaceId },
    /// A CI job of its own `tier`, for `spaces`.
    Job { tier: Tier, spaces: Vec<SpaceId> },
    /// A review of `space`, which a review the daemon left running has not.
    Review { space: Option<SpaceId> },
}

/// A space as ranking sees it.
#[derive(Debug)]
pub(crate) struct Space {
    /// What explanations call it: its number, else its name.
    pub label: String,
    /// Its own tier: `Finishing` with an open PR or a ready mark, `Started`
    /// once an agent has worked in it.
    pub tier: Tier,
    pub steps: Steps,
    pub created_at: UnixTime,
}

/// How many of a space's plan steps are done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Steps {
    pub done: usize,
    pub total: usize,
}

impl Steps {
    /// Compares the shares of steps done, a space without steps counting
    /// as none done.
    fn cmp_share(self, other: Self) -> Ordering {
        let share = |steps: Self, by: Self| steps.done * by.total.max(1);
        share(self, other).cmp(&share(other, self))
    }
}

/// Work that waits or is waited on: a space, or an entry of its own, such
/// as a queued launch that has opened no space yet or a CI job.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Node {
    Space(SpaceId),
    Entry(QueueEntryId),
}

/// `waiting` waits on `on`, so `on` ranks at least as high.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Edge {
    pub waiting: Node,
    pub on: Node,
    pub dependency: Dependency,
}

/// A waiting entry with its rank, in queue order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ranked {
    pub id: QueueEntryId,
    pub rank: QueueRank,
}

/// The waiting entries of `snapshot` in the order they start.
pub(crate) fn rank(snapshot: &Snapshot) -> Vec<Ranked> {
    let mut standing = own_standing(snapshot);
    inherit(&mut standing, snapshot);
    let mut placed: Vec<Placed> = (snapshot.entries.iter())
        .map(|entry| place(entry, &standing, snapshot))
        .collect();
    placed.sort_by(Placed::order);
    placed
        .into_iter()
        .map(|placed| Ranked {
            id: placed.id,
            rank: placed.rank,
        })
        .collect()
}

/// Each node's tier, and the work it inherited it from.
type Standing = HashMap<Node, (Tier, Option<InheritedRank>)>;

/// Each space's own tier, raised by the aged tiers of the agents queued in
/// it; a launch without a space stands alone.
fn own_standing(snapshot: &Snapshot) -> Standing {
    let mut standing: Standing = (snapshot.spaces.iter())
        .map(|(id, space)| (Node::Space(id.clone()), (space.tier, None)))
        .collect();
    for entry in &snapshot.entries {
        let Some(node) = agent_node(entry) else {
            continue;
        };
        let tier = aged(base_tier(entry, snapshot), entry, snapshot.now);
        let held = standing.entry(node).or_insert((tier, None));
        held.0 = held.0.max(tier);
    }
    standing
}

/// Raise each node to the highest tier of the work waiting on it,
/// transitively, remembering what it took that from.
fn inherit(standing: &mut Standing, snapshot: &Snapshot) {
    let mut changed = true;
    // Each pass raises some node a tier, so passes end within three per node.
    while changed {
        changed = false;
        for edge in &snapshot.edges {
            let Some(&(tier, _)) = standing.get(&edge.waiting) else {
                continue;
            };
            let raised = Some(InheritedRank {
                from: label(&edge.waiting, snapshot),
                dependency: edge.dependency,
            });
            match standing.entry(edge.on.clone()) {
                hash_map::Entry::Occupied(mut held) if held.get().0 < tier => {
                    *held.get_mut() = (tier, raised);
                }
                hash_map::Entry::Occupied(_) => continue,
                hash_map::Entry::Vacant(vacant) => {
                    vacant.insert((tier, raised));
                }
            }
            changed = true;
        }
    }
}

/// What explanations call `node`.
fn label(node: &Node, snapshot: &Snapshot) -> String {
    match node {
        Node::Space(id) => {
            (snapshot.spaces.get(id)).map_or_else(|| id.to_string(), |space| space.label.clone())
        }
        Node::Entry(id) => (snapshot.entries.iter())
            .find(|entry| entry.id == *id)
            .map_or_else(|| id.to_string(), |entry| entry.name.clone()),
    }
}

/// The node of an agent's entry: its space, or the launch itself.
fn agent_node(entry: &Entry) -> Option<Node> {
    match &entry.work {
        Work::Launch { space: Some(space) } | Work::Restore { space } => {
            Some(Node::Space(space.clone()))
        }
        Work::Launch { space: None } => Some(Node::Entry(entry.id.clone())),
        Work::Job { .. } | Work::Review { .. } => None,
    }
}

/// The tier of `work` on its own, before it waits or inherits.
fn own_tier(work: &Work) -> Tier {
    match work {
        Work::Launch { .. } | Work::Review { .. } => Tier::New,
        Work::Restore { .. } => Tier::Started,
        Work::Job { tier, .. } => *tier,
    }
}

/// The tier `entry` ages from: its own, or the own tier of the space its
/// work belongs to or serves when that is higher. A tier taken from work
/// waiting on it does not age again.
fn base_tier(entry: &Entry, snapshot: &Snapshot) -> Tier {
    let spaces: &[SpaceId] = match &entry.work {
        Work::Launch { space } | Work::Review { space } => space.as_slice(),
        Work::Restore { space } => std::slice::from_ref(space),
        Work::Job { spaces, .. } => spaces,
    };
    (spaces.iter())
        .filter_map(|id| snapshot.spaces.get(id))
        .map(|space| space.tier)
        .fold(own_tier(&entry.work), Tier::max)
}

/// `tier` risen one level for each full hour `entry` has waited by `now`.
fn aged(tier: Tier, entry: &Entry, now: UnixTime) -> Tier {
    let hours = now.duration_since(entry.queued_at).as_secs() / RISE_AFTER.as_secs();
    (0..hours).fold(tier, |tier, _| match tier {
        Tier::New => Tier::Started,
        Tier::Started | Tier::Finishing => Tier::Finishing,
    })
}

/// An entry with what orders it.
struct Placed {
    id: QueueEntryId,
    rank: QueueRank,
    steps: Steps,
    started: UnixTime,
    /// A CI job or review, which goes ahead of an agent of its space.
    serves: bool,
    arrival: usize,
}

impl Placed {
    fn order(&self, other: &Self) -> Ordering {
        (other.rank.tier.cmp(&self.rank.tier))
            .then_with(|| other.steps.cmp_share(self.steps))
            .then_with(|| self.started.cmp(&other.started))
            .then_with(|| other.serves.cmp(&self.serves))
            .then_with(|| self.arrival.cmp(&other.arrival))
    }
}

/// `entry` with its rank and the space whose progress and age order it
/// within its tier.
fn place(entry: &Entry, standing: &Standing, snapshot: &Snapshot) -> Placed {
    let own = QueueRank {
        tier: aged(own_tier(&entry.work), entry, snapshot.now),
        inherited: None,
    };
    let (rank, space) = match &entry.work {
        Work::Job { spaces, .. } => served(own, spaces, standing, snapshot),
        Work::Review { space } => served(own, space.as_slice(), standing, snapshot),
        Work::Launch { .. } | Work::Restore { .. } => {
            let node = agent_node(entry).expect("an agent's entry has a node");
            let rank = raised(own, standing.get(&node));
            let space = match node {
                Node::Space(id) => snapshot.spaces.get(&id),
                Node::Entry(_) => None,
            };
            (rank, space)
        }
    };
    // Waiting raises it from the tier of its space as well as its own.
    let waited = aged(base_tier(entry, snapshot), entry, snapshot.now);
    let rank = raised(rank.clone(), Some(&(waited, rank.inherited)));
    // Work may wait on the entry itself, such as on a CI run's job.
    let rank = raised(rank, standing.get(&Node::Entry(entry.id.clone())));
    Placed {
        id: entry.id.clone(),
        rank,
        steps: space.map(|space| space.steps).unwrap_or_default(),
        started: space.map_or(entry.queued_at, |space| space.created_at),
        serves: matches!(entry.work, Work::Job { .. } | Work::Review { .. }),
        arrival: entry.arrival,
    }
}

/// `rank`, or `held` where that is a higher tier.
fn raised(rank: QueueRank, held: Option<&(Tier, Option<InheritedRank>)>) -> QueueRank {
    match held {
        Some((tier, inherited)) if *tier > rank.tier => QueueRank {
            tier: *tier,
            inherited: inherited.clone(),
        },
        _ => rank,
    }
}

/// The rank of a CI job or review of rank `own` that serves `spaces`: that
/// of the space among them that orders first, when above its own, with
/// that space.
fn served<'a>(
    own: QueueRank,
    spaces: &[SpaceId],
    standing: &Standing,
    snapshot: &'a Snapshot,
) -> (QueueRank, Option<&'a Space>) {
    let tier = |id: &SpaceId| {
        (standing.get(&Node::Space(id.clone()))).map_or(Tier::New, |(tier, _)| *tier)
    };
    let best = (spaces.iter())
        .map(|id| (id, tier(id), snapshot.spaces.get(id)))
        .min_by(|a, b| first_space((a.1, a.2), (b.1, b.2)));
    let space = best.and_then(|(_, _, space)| space);
    let rank = match best {
        Some((id, tier, _)) if tier > own.tier => QueueRank {
            tier,
            inherited: Some(InheritedRank {
                from: label(&Node::Space(id.clone()), snapshot),
                dependency: Dependency::Serves,
            }),
        },
        _ => own,
    };
    (rank, space)
}

/// Which of two spaces at their tiers orders first, as their agents would:
/// the higher tier, then the larger share of steps done, then the older.
fn first_space(a: (Tier, Option<&Space>), b: (Tier, Option<&Space>)) -> Ordering {
    let steps = |space: Option<&Space>| space.map(|space| space.steps).unwrap_or_default();
    // A space the snapshot lacks goes after every known one.
    let created = |space: Option<&Space>| (space.is_none(), space.map(|space| space.created_at));
    (b.0.cmp(&a.0))
        .then_with(|| steps(b.1).cmp_share(steps(a.1)))
        .then_with(|| created(a.1).cmp(&created(b.1)))
}

#[cfg(test)]
mod tests;
