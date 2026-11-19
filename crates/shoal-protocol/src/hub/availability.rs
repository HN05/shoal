//! How available a machine joined to a hub is: whether lasting work may
//! count on it, which the hub guesses from how reachable the machine was
//! unless `[machine] availability` sets it.
use std::fmt;

use serde::{Deserialize, Serialize};
use shoal_states::states;

states! {
    /// How available a machine is.
    Availability {
        /// It stays reachable, so lasting work may run there.
        AlwaysOn => "always_on",
        /// It comes and goes, as a laptop does.
        Intermittent => "intermittent",
    }
}

states! {
    /// `[machine] availability`: what a machine says of itself, or `auto`
    /// for the hub's guess.
    #[derive(Default)]
    AvailabilitySetting {
        #[default]
        Auto => "auto",
        AlwaysOn => "always_on",
        Intermittent => "intermittent",
    }
}

impl AvailabilitySetting {
    /// The availability the setting names, none for the hub's guess.
    pub fn set(self) -> Option<Availability> {
        match self {
            Self::Auto => None,
            Self::AlwaysOn => Some(Availability::AlwaysOn),
            Self::Intermittent => Some(Availability::Intermittent),
        }
    }
}

/// Why a machine has the availability it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AvailabilityReason {
    /// `[machine] availability` sets it.
    Set,
    /// The hub has seen the machine for less than a day, and goes by
    /// whether it has a battery, none while it never said.
    New { battery: Option<bool> },
    /// The share of the last week the machine was connected, in whole
    /// percent, not counting short gaps.
    Connected { percent: u8 },
}

impl fmt::Display for AvailabilityReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Set => f.write_str("set"),
            Self::New {
                battery: Some(true),
            } => f.write_str("new, battery"),
            Self::New {
                battery: Some(false),
            } => f.write_str("new, no battery"),
            Self::New { battery: None } => f.write_str("new"),
            Self::Connected { percent } => write!(f, "connected {percent}% of 7 days"),
        }
    }
}

/// A machine's availability and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classified {
    pub availability: Availability,
    #[serde(flatten)]
    pub reason: AvailabilityReason,
}

impl fmt::Display for Classified {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.availability, self.reason)
    }
}

/// What a machine tells the hub of itself when its link opens, and again
/// when its setting changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Description {
    pub battery: bool,
    pub availability: AvailabilitySetting,
}

#[cfg(test)]
mod tests;
