//! The order Shoal stops running work in to free resources, what a stop
//! loses least first: CI jobs, newest first, since a rerun loses nothing
//! and the newest has done the least; then demos, which Shoal restarts;
//! then agents, each only after every running agent below its space, so a
//! coordinator never stops while its children work. Among the agents free
//! to go, heavy ones go before light ones, then the one the user interacted
//! with longest ago or never, an idle one before a working one, the space
//! ranked last by the queue, and within a space the one idle longest,
//! otherwise the newest. Shoal's own processes are never in it.
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_time::UnixTime;

/// The running work a stop may free resources from, with the space tree
/// that orders its agents.
#[derive(Debug, Default)]
pub struct Running {
    pub jobs: Vec<RunningJob>,
    pub demos: Vec<RunningDemo>,
    pub agents: Vec<RunningAgent>,
    /// The parent of each space that has one.
    pub parents: HashMap<SpaceId, SpaceId>,
    /// The spaces whose agent may run out of the daemon's reach, such as
    /// one whose wrapper has yet to reconnect after a daemon restart: no
    /// agent above one is in the order, since it cannot stop first.
    pub unreachable: HashSet<SpaceId>,
}

/// A CI job running in a workspace of its own.
#[derive(Debug)]
pub struct RunningJob {
    pub space: SpaceId,
    pub started: UnixTime,
}

/// A running demo.
#[derive(Debug)]
pub struct RunningDemo {
    pub execution: ExecutionId,
    pub space: SpaceId,
    pub started: UnixTime,
}

/// A running agent, a workspace's or a chat's.
#[derive(Debug)]
pub struct RunningAgent {
    pub execution: ExecutionId,
    pub space: SpaceId,
    /// Whether its mode is light.
    pub light: bool,
    /// How long it has run, finer than a start time, so that of agents
    /// started in one second the newest still goes first.
    pub running_for: Duration,
    /// When the user last interacted with it.
    pub interacted_at: Option<UnixTime>,
    /// When its idle turn began, while it is between turns.
    pub idle_since: Option<UnixTime>,
}

/// One entry of the stop order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Stop {
    /// The space whose work it stops.
    pub space: SpaceId,
    pub work: Work,
}

/// What a stop ends.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Work {
    /// The CI job of the stop's workspace.
    Job,
    Demo(ExecutionId),
    Agent {
        execution: ExecutionId,
        light: bool,
    },
}

impl Stop {
    /// The agent execution it stops, if it stops one.
    pub fn agent(&self) -> Option<&ExecutionId> {
        match &self.work {
            Work::Agent { execution, .. } => Some(execution),
            Work::Job | Work::Demo(_) => None,
        }
    }
}

/// The running work in the order it stops, first first.
#[derive(Debug, Default)]
pub struct StopOrder {
    stops: Vec<Stop>,
    parents: HashMap<SpaceId, SpaceId>,
}

impl StopOrder {
    /// `running` in stop order, its agents' spaces taking their place
    /// among each other from `ranked`, first to start first; a space it
    /// leaves out ranks last.
    pub fn new(running: &Running, ranked: &[SpaceId]) -> Self {
        let mut jobs: Vec<&RunningJob> = running.jobs.iter().collect();
        jobs.sort_by_key(|job| std::cmp::Reverse(job.started));
        let mut demos: Vec<&RunningDemo> = running.demos.iter().collect();
        demos.sort_by_key(|demo| std::cmp::Reverse(demo.started));
        let jobs = jobs.into_iter().map(|job| Stop {
            space: job.space.clone(),
            work: Work::Job,
        });
        let demos = demos.into_iter().map(|demo| Stop {
            space: demo.space.clone(),
            work: Work::Demo(demo.execution.clone()),
        });
        let agents = leaves_first(running, ranked).into_iter().map(|agent| Stop {
            space: agent.space.clone(),
            work: Work::Agent {
                execution: agent.execution.clone(),
                light: agent.light,
            },
        });
        Self {
            stops: jobs.chain(demos).chain(agents).collect(),
            parents: running.parents.clone(),
        }
    }

    pub fn stops(&self) -> &[Stop] {
        &self.stops
    }

    /// What stopping `stop` stops, in order: for an agent, every running
    /// agent below its space first, leaves first, then the agent itself.
    pub fn with_agents_below<'a>(&'a self, stop: &'a Stop) -> Vec<&'a Stop> {
        let mut stops: Vec<&Stop> = match stop.work {
            Work::Agent { .. } => (self.stops.iter())
                .filter(|other| other.agent().is_some() && self.is_below(&other.space, &stop.space))
                .collect(),
            Work::Job | Work::Demo(_) => Vec::new(),
        };
        stops.push(stop);
        stops
    }

    /// Whether `space` lies strictly below `ancestor` in the space tree.
    pub fn is_below(&self, space: &SpaceId, ancestor: &SpaceId) -> bool {
        ancestors(&self.parents, space).any(|above| above == ancestor)
    }
}

/// The spaces above `space`, nearest first; a cycle ends the walk.
fn ancestors<'a>(
    parents: &'a HashMap<SpaceId, SpaceId>,
    space: &'a SpaceId,
) -> impl Iterator<Item = &'a SpaceId> {
    let mut seen = HashSet::from([space]);
    std::iter::successors(parents.get(space), move |above| parents.get(*above))
        .take_while(move |above| seen.insert(*above))
}

/// The running agents, each after every agent below its space, choosing
/// among those free to go by [`cost`], short of those above an unreachable
/// space.
fn leaves_first<'a>(running: &'a Running, ranked: &[SpaceId]) -> Vec<&'a RunningAgent> {
    let places: HashMap<&SpaceId, usize> = (ranked.iter().enumerate())
        .map(|(place, space)| (space, place))
        .collect();
    let held: HashSet<&SpaceId> = (running.unreachable.iter())
        .flat_map(|space| ancestors(&running.parents, space))
        .collect();
    let mut left: Vec<&RunningAgent> = (running.agents.iter())
        .filter(|agent| !held.contains(&agent.space))
        .collect();
    // How many agents not yet placed run below each space.
    let mut below: HashMap<&SpaceId, usize> = HashMap::new();
    for agent in &left {
        for above in ancestors(&running.parents, &agent.space) {
            *below.entry(above).or_default() += 1;
        }
    }
    let mut order = Vec::with_capacity(left.len());
    while !left.is_empty() {
        let free = (0..left.len())
            .filter(|&at| below.get(&left[at].space).is_none_or(|count| *count == 0))
            .min_by_key(|&at| cost(left[at], &places));
        // The tree has no cycle, so some agent always has none below it.
        let Some(at) = free else {
            order.append(&mut left);
            break;
        };
        let agent = left.swap_remove(at);
        for above in ancestors(&running.parents, &agent.space) {
            if let Some(count) = below.get_mut(above) {
                *count -= 1;
            }
        }
        order.push(agent);
    }
    order
}

/// What a stop of `agent` costs, least first: a light agent's after a heavy
/// one's, then the user's interaction with it, longest ago or never first,
/// then an idle one before a working one, then the space the queue would
/// start last, then the one idle longest, otherwise the newest.
fn cost(agent: &RunningAgent, places: &HashMap<&SpaceId, usize>) -> impl Ord + use<> {
    let place = places.get(&agent.space).copied().unwrap_or(usize::MAX);
    (
        agent.light,
        agent.interacted_at,
        agent.idle_since.is_none(),
        std::cmp::Reverse(place),
        agent.idle_since,
        agent.running_for,
    )
}

#[cfg(test)]
mod tests;
