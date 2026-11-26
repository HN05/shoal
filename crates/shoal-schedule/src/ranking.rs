//! The ranking the built-in schedules share, which differ only in their
//! [`Lead`]. `docs/design/queue.md` gives the order and why.
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
    stopping::StopOrder,
};

/// How long an entry waits to rise one tier.
const RISE_AFTER: Duration = Duration::from_secs(60 * 60);

/// What a schedule puts first within a priority, ahead of the tier.
pub trait Lead {
    /// What the lead reads of the whole snapshot, once per ranking.
    type Context;
    /// A higher key goes first.
    type Key: Copy + Ord;

    fn context(&self, snapshot: &Snapshot) -> Self::Context;

    fn key(&self, entry: &Entry, context: &Self::Context, snapshot: &Snapshot) -> Self::Key;
}

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
                name: placed.name,
                rank: placed.rank,
            })
            .collect()
    }

    /// Ranks each space as its agent's entry would rank, without the lead.
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

    fn stop_order(&self, snapshot: &Snapshot) -> StopOrder {
        let mut spaces: Vec<SpaceId> = (snapshot.running.agents.iter())
            .map(|agent| agent.space.clone())
            .collect();
        spaces.sort();
        spaces.dedup();
        let ranked: Vec<SpaceId> = (self.spaces_in_order(snapshot, &spaces).into_iter())
            .map(|(id, _)| id)
            .collect();
        StopOrder::new(&snapshot.running, &ranked)
    }
}

/// Each node's rank, with the work it inherited each part from.
type Standing = HashMap<Node, QueueRank>;

fn standing(snapshot: &Snapshot) -> Standing {
    let mut standing = own_standing(snapshot);
    inherit(&mut standing, snapshot);
    standing
}

/// A rank that inherits nothing.
fn own(priority: Priority, tier: Tier) -> QueueRank {
    QueueRank {
        priority,
        priority_from: None,
        tier,
        tier_from: None,
        shared_areas: Vec::new(),
    }
}

/// Each node's rank before it inherits. A space takes the aged tier of its
/// queued agents when that is higher.
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

fn has_priority(node: &Node, snapshot: &Snapshot) -> bool {
    let Node::Space(id) = node else {
        return false;
    };
    (snapshot.spaces.get(id)).is_some_and(|space| space.priority.is_some())
}

/// Raise each node to the highest rank of the work waiting on it,
/// transitively.
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

/// The rank `edge` passes on, none while its waiting work has no rank.
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

/// The priority a chat passes its children: the higher of its own and what
/// its parent passes it. A chat's tier never passes on.
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

/// Raise `held` to each part of `by` that is higher. Returns whether
/// anything rose.
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
        Work::Launch { .. } | Work::OneShot { .. } => Tier::New,
        Work::Restore { .. } => Tier::Started,
        Work::Job { tier, .. } => *tier,
    }
}

/// The tier `entry` ages from: its own, or its spaces' own tier when that
/// is higher. A tier inherited from waiting work never ages again.
fn base_tier(entry: &Entry, snapshot: &Snapshot) -> Tier {
    let spaces: &[SpaceId] = match &entry.work {
        Work::Launch { space } | Work::OneShot { space } => space.as_slice(),
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

struct Placed<K> {
    id: QueueEntryId,
    name: String,
    rank: QueueRank,
    lead: K,
    /// Whether it has waited long enough to rise a tier.
    overdue: bool,
    shared: usize,
    steps: Steps,
    started: UnixTime,
    /// A CI job or review, which goes ahead of its space's agent.
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

fn place<K>(entry: &Entry, lead: K, standing: &Standing, snapshot: &Snapshot) -> Placed<K> {
    let tier = aged(own_tier(&entry.work), entry, snapshot.now);
    let (mut rank, space) = match &entry.work {
        Work::Job { spaces, .. } => served(tier, spaces, standing, snapshot),
        Work::OneShot { space } => served(tier, space.as_slice(), standing, snapshot),
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
        name: entry.name.clone(),
        lead,
        overdue: snapshot.now.duration_since(entry.queued_at) >= RISE_AFTER,
        shared: rank.shared_areas.len(),
        rank,
        steps: space.map(|space| space.steps).unwrap_or_default(),
        started: space.map_or(entry.queued_at, |space| space.created_at),
        serves: matches!(entry.work, Work::Job { .. } | Work::OneShot { .. }),
        arrival: entry.arrival,
    }
}

/// The areas a new workspace's launch shares with its repository's other
/// workspaces.
fn shared_areas(entry: &Entry, snapshot: &Snapshot) -> Vec<SharedArea> {
    let Work::Launch { space: Some(space) } = &entry.work else {
        return Vec::new();
    };
    let occupied = (entry.repository.as_ref()).and_then(|id| snapshot.occupied.get(id));
    let Some(occupied) = occupied.filter(|_| is_new(space, snapshot)) else {
        return Vec::new();
    };
    (entry.areas.iter())
        .filter_map(|area| {
            let others = (occupied.get(area)?.iter()).filter(|holder| *holder != space);
            let spaces: Vec<_> = others
                .map(|id| label(&Node::Space(id.clone()), snapshot))
                .collect();
            (!spaces.is_empty()).then(|| SharedArea {
                area: area.clone(),
                spaces,
            })
        })
        .collect()
}

fn is_new(space: &SpaceId, snapshot: &Snapshot) -> bool {
    (snapshot.spaces.get(space)).is_some_and(|space| space.tier == Tier::New)
}

/// Give each overdue entry the highest lead of its priority, so that no
/// schedule starves it. Its tier still orders it among the entries it ties.
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

/// The rank of an agent's `entry`, with the space it starts in.
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

/// The rank of a CI job or review that serves `spaces`: the priority of the
/// space that orders first, and its tier when higher, with that space.
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

/// Which of two ranked spaces orders first, as their agents would.
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
