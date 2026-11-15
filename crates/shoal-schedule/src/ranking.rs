//! The ranking the built-in schedules share, which decides what starts
//! first. Each entry has a priority, how much its work matters, which goes
//! first, and a tier, how close its work is to finished, which rises one
//! level for each hour it waits. Both flow along the dependency graph, from
//! work that waits to the work it waits on, so that an entry ranks at least
//! as high as anything waiting on it; a CI job or review ranks with the
//! spaces it serves. Within a priority a schedule's [`Lead`] may put other
//! work first. Within a tier the launch of a new workspace that shares
//! fewer areas with its repository's workspaces goes first, then the larger
//! share of a space's plan steps done, then the space started earliest,
//! then a CI job or review ahead of an agent, then arrival.
use std::{
    cmp::Ordering,
    collections::{HashMap, hash_map},
    time::Duration,
};

use crate::{
    priority::Priority,
    rank::{Dependency, InheritedRank, QueueRank, SharedArea, Tier},
};
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_time::UnixTime;

use super::{
    scheduler::{Ranked, Scheduler},
    snapshot::{Carries, Edge, Entry, Node, Snapshot, Space, Steps, Work},
};

/// How long an entry waits to rise one tier.
const RISE_AFTER: Duration = Duration::from_secs(60 * 60);

/// What a schedule puts first within a priority, ahead of the tier: the
/// part of the order the built-in schedules differ in.
pub trait Lead {
    /// What it reads of the whole snapshot once, before it leads any entry.
    type Context;
    /// What an entry leads with, higher first.
    type Key: Copy + Ord;

    fn context(&self, snapshot: &Snapshot) -> Self::Context;

    /// What `entry` leads with, from the snapshot's `context`.
    fn key(&self, entry: &Entry, context: &Self::Context, snapshot: &Snapshot) -> Self::Key;
}

/// The shared ranking, leading within a priority with what `L` puts first.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ranking<L> {
    lead: L,
}

impl<L> Ranking<L> {
    pub const fn new(lead: L) -> Self {
        Self { lead }
    }
}

impl<L: Lead> Scheduler for Ranking<L> {
    fn rank(&self, snapshot: &Snapshot) -> Vec<Ranked> {
        let standing = standing(snapshot);
        let context = self.lead.context(snapshot);
        let mut placed: Vec<Placed<L::Key>> = (snapshot.entries.iter())
            .map(|entry| {
                let lead = self.lead.key(entry, &context, snapshot);
                place(entry, lead, &standing, snapshot)
            })
            .collect();
        lead_overdue(&mut placed);
        placed.sort_by(Placed::order);
        placed
            .into_iter()
            .map(|placed| Ranked {
                id: placed.id,
                rank: placed.rank,
            })
            .collect()
    }

    /// Each space at its own rank raised by the work that waits on it, as
    /// entries rank, whatever leads.
    fn spaces_in_order(&self, snapshot: &Snapshot, ids: &[SpaceId]) -> Vec<(SpaceId, Priority)> {
        let standing = standing(snapshot);
        let mut ranked: Vec<(SpaceId, QueueRank)> = (ids.iter())
            .map(|id| {
                let held = standing.get(&Node::Space(id.clone())).cloned();
                (
                    id.clone(),
                    held.unwrap_or_else(|| own(Priority::default(), Tier::New)),
                )
            })
            .collect();
        let space = |id: &SpaceId| snapshot.spaces.get(id);
        ranked.sort_by(|a, b| first_space((&a.1, space(&a.0)), (&b.1, space(&b.0))));
        (ranked.into_iter())
            .map(|(id, rank)| (id, rank.priority))
            .collect()
    }
}

/// Each node's rank, with the work it inherited each part from.
type Standing = HashMap<Node, QueueRank>;

/// Each node's rank: its own, raised by what waits on it.
fn standing(snapshot: &Snapshot) -> Standing {
    let mut standing = own_standing(snapshot);
    inherit(&mut standing, snapshot);
    standing
}

/// A rank of `priority` and `tier` taken from nothing.
fn own(priority: Priority, tier: Tier) -> QueueRank {
    QueueRank {
        priority,
        priority_from: None,
        tier,
        tier_from: None,
        shared_areas: Vec::new(),
    }
}

/// Each space's own rank, its tier raised by the aged tiers of the agents
/// queued in it, and its priority, when it has none of its own, by theirs;
/// a launch without a space stands alone.
fn own_standing(snapshot: &Snapshot) -> Standing {
    let mut standing: Standing = (snapshot.spaces.iter())
        .map(|(id, space)| {
            let rank = own(space.priority.unwrap_or_default(), space.tier);
            (Node::Space(id.clone()), rank)
        })
        .collect();
    for entry in &snapshot.entries {
        let Some(node) = entry.agent_node() else {
            continue;
        };
        let tier = aged(base_tier(entry, snapshot), entry, snapshot.now);
        let unprioritized = !has_priority(&node, snapshot);
        let held =
            (standing.entry(node)).or_insert_with(|| own(entry.priority.unwrap_or_default(), tier));
        held.tier = held.tier.max(tier);
        // Its space's issue may not have been read yet, so a launch's
        // priority from its labels stands in.
        if let Some(priority) = entry.priority.filter(|_| unprioritized) {
            held.priority = held.priority.max(priority);
        }
    }
    standing
}

/// Whether `node` is a space with a priority of its own.
fn has_priority(node: &Node, snapshot: &Snapshot) -> bool {
    let Node::Space(id) = node else {
        return false;
    };
    (snapshot.spaces.get(id)).is_some_and(|space| space.priority.is_some())
}

/// Raise each node to the highest priority and tier of the work waiting on
/// it, transitively, remembering what it took each from.
fn inherit(standing: &mut Standing, snapshot: &Snapshot) {
    let mut changed = true;
    // Each pass raises some node, and nothing falls, so the passes end.
    while changed {
        changed = false;
        for edge in &snapshot.edges {
            let Some(carried) = carried(edge, standing, snapshot) else {
                continue;
            };
            // Work nothing else ranks, such as a CI job, takes what it
            // carries as it is, a lower priority too.
            match standing.entry(edge.on.clone()) {
                hash_map::Entry::Occupied(mut held) => changed |= raise(held.get_mut(), carried),
                hash_map::Entry::Vacant(vacant) => {
                    vacant.insert(carried);
                    changed = true;
                }
            }
        }
    }
}

/// The rank `edge` carries from its waiting work, taken from it.
fn carried(edge: &Edge, standing: &Standing, snapshot: &Snapshot) -> Option<QueueRank> {
    let from = || {
        Some(InheritedRank {
            from: label(&edge.waiting, snapshot),
            dependency: edge.dependency,
        })
    };
    match edge.carries {
        Carries::Rank => {
            let waiting = standing.get(&edge.waiting)?;
            Some(QueueRank {
                priority_from: from(),
                tier_from: from(),
                ..own(waiting.priority, waiting.tier)
            })
        }
        Carries::SetPriority => {
            let priority = passed_priority(&edge.waiting, standing, snapshot)?;
            Some(QueueRank {
                priority_from: from(),
                ..own(priority, Tier::New)
            })
        }
    }
}

/// The priority a chat passes its children: the higher of its own, set or
/// from its issue's labels, and what its own parent passes it.
fn passed_priority(chat: &Node, standing: &Standing, snapshot: &Snapshot) -> Option<Priority> {
    let Node::Space(id) = chat else {
        return None;
    };
    let set = snapshot.spaces.get(id).and_then(|space| space.priority);
    let from_parent = (snapshot.edges.iter())
        .find(|edge| edge.on == *chat && edge.dependency == Dependency::Parent)
        .and_then(|edge| carried(edge, standing, snapshot));
    set.max(from_parent.map(|rank| rank.priority))
}

/// Raise `held` to each part of `by` that is higher, with what that came
/// from, answering whether anything rose.
fn raise(held: &mut QueueRank, by: QueueRank) -> bool {
    let mut rose = false;
    if by.priority > held.priority {
        held.priority = by.priority;
        held.priority_from = by.priority_from;
        rose = true;
    }
    if by.tier > held.tier {
        held.tier = by.tier;
        held.tier_from = by.tier_from;
        rose = true;
    }
    rose
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

/// An entry with what orders it, leading with a `K`.
struct Placed<K> {
    id: QueueEntryId,
    rank: QueueRank,
    /// What the schedule puts first within a priority, higher first.
    lead: K,
    /// Whether it has waited as long as rises a tier.
    overdue: bool,
    /// How many areas a launch of a new workspace shares with workspaces.
    shared: usize,
    steps: Steps,
    started: UnixTime,
    /// A CI job or review, which goes ahead of an agent of its space.
    serves: bool,
    arrival: usize,
}

impl<K: Ord> Placed<K> {
    fn order(&self, other: &Self) -> Ordering {
        (other.rank.priority.cmp(&self.rank.priority))
            .then_with(|| other.lead.cmp(&self.lead))
            .then_with(|| other.rank.tier.cmp(&self.rank.tier))
            .then_with(|| self.shared.cmp(&other.shared))
            .then_with(|| other.steps.cmp_share(self.steps))
            .then_with(|| self.started.cmp(&other.started))
            .then_with(|| other.serves.cmp(&self.serves))
            .then_with(|| self.arrival.cmp(&other.arrival))
    }
}

/// `entry` leading with `lead`, with its rank and the space whose progress
/// and age order it within its tier.
fn place<K>(entry: &Entry, lead: K, standing: &Standing, snapshot: &Snapshot) -> Placed<K> {
    let tier = aged(own_tier(&entry.work), entry, snapshot.now);
    let (mut rank, space) = match &entry.work {
        Work::Job { spaces, .. } => served(tier, spaces, standing, snapshot),
        Work::Review { space } => served(tier, space.as_slice(), standing, snapshot),
        Work::Launch { .. } | Work::Restore { .. } => agent_rank(entry, tier, standing, snapshot),
    };
    // Waiting raises it from the tier of its space as well as its own.
    let waited = aged(base_tier(entry, snapshot), entry, snapshot.now);
    rank.tier = rank.tier.max(waited);
    // Work may wait on the entry itself, such as on a CI run's job.
    if let Some(held) = standing.get(&Node::Entry(entry.id.clone())) {
        raise(&mut rank, held.clone());
    }
    rank.shared_areas = shared_areas(entry, snapshot);
    Placed {
        id: entry.id.clone(),
        lead,
        overdue: snapshot.now.duration_since(entry.queued_at) >= RISE_AFTER,
        shared: rank.shared_areas.len(),
        rank,
        steps: space.map(|space| space.steps).unwrap_or_default(),
        started: space.map_or(entry.queued_at, |space| space.created_at),
        serves: matches!(entry.work, Work::Job { .. } | Work::Review { .. }),
        arrival: entry.arrival,
    }
}

/// The areas `entry`, when it launches a new workspace, shares with the
/// workspaces of its repository, each with what explanations call them.
fn shared_areas(entry: &Entry, snapshot: &Snapshot) -> Vec<SharedArea> {
    let Work::Launch { space: None } = entry.work else {
        return Vec::new();
    };
    let occupied = (entry.repository.as_ref()).and_then(|id| snapshot.occupied.get(id));
    let Some(occupied) = occupied else {
        return Vec::new();
    };
    (entry.areas.iter())
        .filter_map(|area| {
            let holders = occupied.get(area)?;
            Some(SharedArea {
                area: area.clone(),
                spaces: (holders.iter())
                    .map(|id| label(&Node::Space(id.clone()), snapshot))
                    .collect(),
            })
        })
        .collect()
}

/// Give each overdue entry the highest lead any entry of its priority has,
/// so that no schedule passes it over and nothing starves within its
/// priority, while the tier still orders it among the entries it ties with.
fn lead_overdue<K: Copy + Ord>(placed: &mut [Placed<K>]) {
    let mut top: HashMap<Priority, K> = HashMap::new();
    for placed in placed.iter() {
        (top.entry(placed.rank.priority))
            .and_modify(|lead| *lead = (*lead).max(placed.lead))
            .or_insert(placed.lead);
    }
    for placed in placed.iter_mut().filter(|placed| placed.overdue) {
        placed.lead = top[&placed.rank.priority];
    }
}

/// The rank of the agent `entry` at its own `tier`: its node's priority,
/// and its node's tier when higher, with the space it starts in.
fn agent_rank<'a>(
    entry: &Entry,
    tier: Tier,
    standing: &Standing,
    snapshot: &'a Snapshot,
) -> (QueueRank, Option<&'a Space>) {
    let node = entry.agent_node().expect("an agent's entry has a node");
    let rank = match standing.get(&node) {
        Some(held) => {
            let mut rank = QueueRank {
                priority_from: held.priority_from.clone(),
                ..own(held.priority, tier)
            };
            raise(&mut rank, held.clone());
            rank
        }
        None => own(entry.priority.unwrap_or_default(), tier),
    };
    let space = match node {
        Node::Space(id) => snapshot.spaces.get(&id),
        Node::Entry(_) => None,
    };
    (rank, space)
}

/// The rank of a CI job or review of its own `tier` that serves `spaces`:
/// the priority of the space among them that orders first, and its tier
/// when above its own, with that space. Without one it has the default
/// priority.
fn served<'a>(
    tier: Tier,
    spaces: &[SpaceId],
    standing: &Standing,
    snapshot: &'a Snapshot,
) -> (QueueRank, Option<&'a Space>) {
    let mut rank = own(Priority::default(), tier);
    let held = |id: &SpaceId| {
        (standing.get(&Node::Space(id.clone())).cloned())
            .unwrap_or_else(|| own(Priority::default(), Tier::New))
    };
    let best = (spaces.iter())
        .map(|id| (id, held(id), snapshot.spaces.get(id)))
        .min_by(|a, b| first_space((&a.1, a.2), (&b.1, b.2)));
    let Some((id, best, space)) = best else {
        return (rank, None);
    };
    let from = Some(InheritedRank {
        from: label(&Node::Space(id.clone()), snapshot),
        dependency: Dependency::Serves,
    });
    if best.priority != rank.priority {
        rank.priority = best.priority;
        rank.priority_from = from.clone();
    }
    if best.tier > rank.tier {
        rank.tier = best.tier;
        rank.tier_from = from;
    }
    (rank, space)
}

/// Which of two spaces at their ranks orders first, as their agents would:
/// the higher priority, then the higher tier, then the larger share of
/// steps done, then the older.
fn first_space(a: (&QueueRank, Option<&Space>), b: (&QueueRank, Option<&Space>)) -> Ordering {
    let steps = |space: Option<&Space>| space.map(|space| space.steps).unwrap_or_default();
    // A space the snapshot lacks goes after every known one.
    let created = |space: Option<&Space>| (space.is_none(), space.map(|space| space.created_at));
    (b.0.priority.cmp(&a.0.priority))
        .then_with(|| b.0.tier.cmp(&a.0.tier))
        .then_with(|| steps(b.1).cmp_share(steps(a.1)))
        .then_with(|| created(a.1).cmp(&created(b.1)))
}

#[cfg(test)]
mod tests;
