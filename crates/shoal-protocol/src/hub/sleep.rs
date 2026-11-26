//! What a machine tells the hub of its power and its sleep.
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::SpaceId;

use crate::time::UnixTime;

/// How a machine is powered as it reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Power {
    /// Whether it runs on its battery, not on external power.
    pub on_battery: bool,
    /// How full its battery is, in whole percent.
    pub percent: u8,
}

impl Power {
    /// The battery level below which a machine on battery that drops
    /// counts as one that announced its sleep, as it will have run out.
    pub const NEARLY_EMPTY: u8 = 5;

    /// Whether the machine is about to run out: on battery below
    /// [`Self::NEARLY_EMPTY`].
    pub fn nearly_empty(&self) -> bool {
        self.on_battery && self.percent < Self::NEARLY_EMPTY
    }
}

/// A space whose package the hub keeps lacks changes as its machine goes
/// to sleep: since when, none where the machine cannot tell, as one that
/// keeps no packages current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lagging {
    pub space: SpaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<UnixTime>,
}

impl Lagging {
    /// This lag with its time on the hub's clock, as `clocks` carry it over.
    pub fn on_hub_clock(self, clocks: Clocks) -> Self {
        Self {
            since: self.since.map(|since| clocks.on_hub(since)),
            ..self
        }
    }
}

/// When a machine sent a call, by its own clock, and when the hub took
/// it, by the hub's: what carries the times the call names over to the
/// hub's clock, so a machine whose clock differs from the hub's names the
/// same moments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clocks {
    pub sent: UnixTime,
    pub received: UnixTime,
}

impl Clocks {
    /// `time`, on the machine's clock no later than it sent the call, as
    /// the hub's clock reads it.
    pub fn on_hub(self, time: UnixTime) -> UnixTime {
        self.received - self.sent.duration_since(time)
    }
}

#[cfg(test)]
mod tests;
