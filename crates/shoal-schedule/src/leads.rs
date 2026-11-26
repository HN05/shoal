//! The leads of the built-in schedules.
use std::collections::HashMap;

use shoal_time::UnixTime;

use super::{
    ranking::Lead,
    snapshot::{Entry, Node, Snapshot, Work},
};

mod chains;

/// Nothing goes ahead of the tier: the `balanced` schedule.
#[derive(Debug, Clone, Copy, Default)]
pub struct Nothing;

impl Lead for Nothing {
    type Context = ();
    type Key = ();

    fn context(&self, _: &Snapshot) {}

    fn key(&self, _: &Entry, _: &(), _: &Snapshot) {}
}

/// The work with the longest chain of work waiting on it goes first: the
/// `speed` schedule.
#[derive(Debug, Clone, Copy, Default)]
pub struct LongestChain;

impl Lead for LongestChain {
    /// The length of the longest chain of work waiting on each node.
    type Context = HashMap<Node, usize>;
    type Key = usize;

    fn context(&self, snapshot: &Snapshot) -> Self::Context {
        chains::chains(&snapshot.edges)
    }

    /// The longest chain of work waiting on `entry`. The spaces a CI job or
    /// review serves wait on it, so their chains count one link more.
    fn key(&self, entry: &Entry, chains: &Self::Context, _: &Snapshot) -> usize {
        let chain = |node: &Node| chains.get(node).copied().unwrap_or(0);
        let served = match &entry.work {
            Work::Job { spaces, .. } => spaces.clone(),
            Work::OneShot { space } => space.iter().cloned().collect(),
            Work::Launch { .. } | Work::Restore { .. } => Vec::new(),
        };
        let own = (entry.agent_node()).unwrap_or_else(|| Node::Entry(entry.id.clone()));
        (served.into_iter())
            .map(|space| 1 + chain(&Node::Space(space)))
            .fold(chain(&own), usize::max)
    }
}

/// Restores whose agent's prompt cache is still warm go first, the latest
/// to expire first: the `cache` schedule.
#[derive(Debug, Clone, Copy, Default)]
pub struct WarmCache;

impl Lead for WarmCache {
    type Context = ();
    /// When a restore's warm prompt cache expires.
    type Key = Option<UnixTime>;

    fn context(&self, _: &Snapshot) {}

    fn key(&self, entry: &Entry, _: &(), snapshot: &Snapshot) -> Option<UnixTime> {
        match (&entry.work, entry.prompt_cache_until) {
            (Work::Restore { .. }, Some(until)) if until > snapshot.now => Some(until),
            _ => None,
        }
    }
}
