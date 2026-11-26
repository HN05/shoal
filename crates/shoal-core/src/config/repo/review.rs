//! The `[review]` section, and the review gates it requires.
use crate::agent::Agent;
use crate::config::options::Options;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Review)]
pub struct ReviewConfig {
    #[option(default = true)]
    pub post: Option<bool>,
    #[serde(deserialize_with = "crate::agent::configured")]
    pub agent: Option<Agent>,
    #[serde(deserialize_with = "crate::agent::configured")]
    pub fallback_agent: Option<Agent>,
    #[option(default = ReviewRequirement::default())]
    pub require_for_pr: Option<ReviewRequirement>,
    #[option(default = ReviewRequirement::default())]
    pub require_for_ready: Option<ReviewRequirement>,
    #[option(default = ReviewRequirement::default())]
    pub require_for_merge: Option<ReviewRequirement>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewRequirement {
    #[default]
    Off,
    Agents,
    All,
}

/// [`ReviewRequirement`]'s values, which a refusal of any other names.
pub(super) const REVIEW_REQUIREMENTS: &[&str] = &["off", "agents", "all"];

impl<'de> Deserialize<'de> for ReviewRequirement {
    /// Refuses any other value, a boolean too, naming those it takes.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Values;
        impl serde::de::Visitor<'_> for Values {
            type Value = ReviewRequirement;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(formatter, "one of {}", REVIEW_REQUIREMENTS.join(", "))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                match value {
                    "off" => Ok(ReviewRequirement::Off),
                    "agents" => Ok(ReviewRequirement::Agents),
                    "all" => Ok(ReviewRequirement::All),
                    other => Err(E::unknown_variant(other, REVIEW_REQUIREMENTS)),
                }
            }
        }
        deserializer.deserialize_any(Values)
    }
}

/// Who runs a command a review gate guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requester {
    /// The user, from the user's own terminal.
    User,
    /// An agent or a command it runs, as a scoped caller.
    Agent,
}

impl ReviewRequirement {
    /// Whether it holds a command `requester` runs.
    pub fn holds(self, requester: Requester) -> bool {
        match self {
            Self::Off => false,
            Self::Agents => requester == Requester::Agent,
            Self::All => true,
        }
    }
}
