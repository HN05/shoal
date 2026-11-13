//! The rank of the queue's waiting work, which decides what starts first.
//! Each entry has a priority, how much its work matters, which goes first,
//! and a tier, how close its work is to finished, which rises one level for
//! each hour it waits. Both flow along the dependency graph, from work that
//! waits to the work it waits on, so that an entry ranks at least as high as
//! anything waiting on it; a CI job or review ranks with the spaces it
//! serves. Within a priority the schedule may put other work first: the
//! work with the longest chain of work waiting on it, or restores whose
//! prompt cache is still warm. Within a tier the larger share of a space's
//! plan steps done goes first, then the space started earliest, then a CI
//! job or review ahead of an agent, then arrival.
use std::{
    cmp::Ordering,
    collections::{HashMap, hash_map},
    time::Duration,
};

use crate::client::{priority::Priority, queue::QueueEntryId, space_ids::SpaceId, time::UnixTime};
use crate::config::machine::Schedule;
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
    /// What goes first within a priority.
    pub schedule: Schedule,
}

/// A waiting entry.
#[derive(Debug)]
pub(crate) struct Entry {
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
    /// The priority set on it, else the highest its issue's labels map to;
    /// it has the default without one.
    pub priority: Option<Priority>,
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

/// `waiting` waits on `on`, so `on` ranks at least as high as what the
/// edge carries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Edge {
    pub waiting: Node,
    pub on: Node,
    pub dependency: Dependency,
    pub carries: Carries,
}

/// What of the waiting work's rank an edge passes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Carries {
    /// Its priority and tier, its own or inherited.
    Rank,
    /// Only a priority of its space's own, set or from its issue's labels,
    /// or passed down to it, as a chat passes to its children, which
    /// compete with every other worker.
    SetPriority,
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
    let chains = schedule_chains(snapshot);
    let mut placed: Vec<Placed> = (snapshot.entries.iter())
        .map(|entry| place(entry, &standing, &chains, snapshot))
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

/// Each node's rank, with the work it inherited each part from.
type Standing = HashMap<Node, QueueRank>;

/// A rank of `priority` and `tier` taken from nothing.
fn own(priority: Priority, tier: Tier) -> QueueRank {
    QueueRank {
        priority,
        priority_from: None,
        tier,
        tier_from: None,
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
        let Some(node) = agent_node(entry) else {
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
                priority: waiting.priority,
                priority_from: from(),
                tier: waiting.tier,
                tier_from: from(),
            })
        }
        Carries::SetPriority => {
            let priority = passed_priority(&edge.waiting, standing, snapshot)?;
            Some(QueueRank {
                priority,
                priority_from: from(),
                tier: Tier::New,
                tier_from: None,
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
    /// What the schedule puts first within a priority, higher first.
    lead: Lead,
    steps: Steps,
    started: UnixTime,
    /// A CI job or review, which goes ahead of an agent of its space.
    serves: bool,
    arrival: usize,
}

impl Placed {
    fn order(&self, other: &Self) -> Ordering {
        (other.rank.priority.cmp(&self.rank.priority))
            .then_with(|| other.lead.cmp(&self.lead))
            .then_with(|| other.rank.tier.cmp(&self.rank.tier))
            .then_with(|| other.steps.cmp_share(self.steps))
            .then_with(|| self.started.cmp(&other.started))
            .then_with(|| other.serves.cmp(&self.serves))
            .then_with(|| self.arrival.cmp(&other.arrival))
    }
}

/// `entry` with its rank and the space whose progress and age order it
/// within its tier.
fn place(
    entry: &Entry,
    standing: &Standing,
    chains: &HashMap<Node, usize>,
    snapshot: &Snapshot,
) -> Placed {
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
    Placed {
        id: entry.id.clone(),
        rank,
        lead: lead(entry, chains, snapshot),
        steps: space.map(|space| space.steps).unwrap_or_default(),
        started: space.map_or(entry.queued_at, |space| space.created_at),
        serves: matches!(entry.work, Work::Job { .. } | Work::Review { .. }),
        arrival: entry.arrival,
    }
}

/// What a schedule puts first within a priority: under `speed` the length
/// of the longest chain of work waiting on the entry, under `cache` when a
/// restore's warm prompt cache expires.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Lead {
    #[default]
    None,
    Chain(usize),
    Cache(UnixTime),
}

/// The chains of work waiting on each node, which only `speed` reads.
fn schedule_chains(snapshot: &Snapshot) -> HashMap<Node, usize> {
    match snapshot.schedule {
        Schedule::Speed => chains::chains(&snapshot.edges),
        Schedule::Balanced | Schedule::Cache => HashMap::new(),
    }
}

/// What `entry` leads with under the snapshot's schedule, from the `chains`
/// of work waiting on each node.
fn lead(entry: &Entry, chains: &HashMap<Node, usize>, snapshot: &Snapshot) -> Lead {
    match snapshot.schedule {
        Schedule::Balanced => Lead::None,
        Schedule::Speed => Lead::Chain(chain_behind(entry, chains)),
        Schedule::Cache => match (&entry.work, entry.prompt_cache_until) {
            (Work::Restore { .. }, Some(until)) if until > snapshot.now => Lead::Cache(until),
            _ => Lead::None,
        },
    }
}

/// The longest chain of work waiting on `entry`: on its agent's node, or
/// for a CI job or review on itself or the spaces it serves, which wait on
/// it.
fn chain_behind(entry: &Entry, chains: &HashMap<Node, usize>) -> usize {
    let chain = |node: &Node| chains.get(node).copied().unwrap_or(0);
    let served = match &entry.work {
        Work::Job { spaces, .. } => spaces.clone(),
        Work::Review { space } => space.iter().cloned().collect(),
        Work::Launch { .. } | Work::Restore { .. } => Vec::new(),
    };
    let own = agent_node(entry).unwrap_or_else(|| Node::Entry(entry.id.clone()));
    (served.into_iter())
        .map(|space| 1 + chain(&Node::Space(space)))
        .fold(chain(&own), usize::max)
}

/// The rank of the agent `entry` at its own `tier`: its node's priority,
/// and its node's tier when higher, with the space it starts in.
fn agent_rank<'a>(
    entry: &Entry,
    tier: Tier,
    standing: &Standing,
    snapshot: &'a Snapshot,
) -> (QueueRank, Option<&'a Space>) {
    let node = agent_node(entry).expect("an agent's entry has a node");
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

mod chains;

#[cfg(test)]
mod tests;
