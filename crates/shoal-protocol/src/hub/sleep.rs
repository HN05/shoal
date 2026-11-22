//! What a machine tells the hub of its sleep: how it is powered, with its
//! reports, and the notice it sends as it goes to sleep, after which the
//! hub may take its spaces over once it has stayed away long enough.
use serde::{Deserialize, Serialize};

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
