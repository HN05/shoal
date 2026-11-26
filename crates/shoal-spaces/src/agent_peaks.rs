//! How far each space's agent rose above what it held as its latest tool
//! call began, outside the commands its space runs, as usage samples show
//! it: a rise of [`NUDGE_ABOVE`] means it ran a build or test directly
//! rather than through `shoal run`, which its hook then tells it, at most
//! once every [`NUDGE_INTERVAL`].
use std::{collections::HashMap, time::Duration};

use shoal_ids::space_ids::SpaceId;
use tokio::time::Instant;

/// How far an agent's own memory rises during a tool call before Shoal
/// tells it to build through `shoal run`.
pub const NUDGE_ABOVE: u64 = 2 << 30;

/// How long a space's agent goes untold after Shoal told it.
pub const NUDGE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// The agents' memory since each space's latest tool call ended.
#[derive(Debug, Default)]
pub struct AgentPeaks {
    spaces: HashMap<SpaceId, Held>,
}

/// What a space's agent held outside its commands.
#[derive(Debug, Default)]
struct Held {
    /// At the sample before its latest tool call began.
    base: Option<u64>,
    /// At the latest sample.
    latest: u64,
    /// The most, since its latest tool call began.
    peak: u64,
    /// When Shoal last told it.
    told: Option<Instant>,
}

impl AgentPeaks {
    /// Follow a sample in which each space's agent held `held` outside its
    /// commands, forgetting the spaces it no longer shows.
    pub fn observe(&mut self, held: impl IntoIterator<Item = (SpaceId, u64)>) {
        let held: HashMap<SpaceId, u64> = held.into_iter().collect();
        self.spaces.retain(|space, _| held.contains_key(space));
        for (space, memory) in held {
            let entry = self.spaces.entry(space).or_default();
            entry.latest = memory;
            entry.peak = entry.peak.max(memory);
        }
    }

    /// As a tool call of the agent of `space` ends at `now`, how far its own
    /// memory rose above its base during the call when that is a build's
    /// worth and it was not told within [`NUDGE_INTERVAL`]; the next call
    /// starts from what it holds now.
    pub fn rise_after_call(&mut self, space: &SpaceId, now: Instant) -> Option<u64> {
        let held = self.spaces.get_mut(space)?;
        let rise = (held.base).map(|base| held.peak.saturating_sub(base));
        held.base = Some(held.latest);
        held.peak = held.latest;
        let told_lately = (held.told).is_some_and(|told| now.duration_since(told) < NUDGE_INTERVAL);
        let rise = rise.filter(|rise| *rise > NUDGE_ABOVE && !told_lately)?;
        held.told = Some(now);
        Some(rise)
    }
}

#[cfg(test)]
mod tests;
