//! The state of a CI check, which every forge's checks and commit statuses
//! map onto.
use serde::{Deserialize, Deserializer};

use crate::state::states;

states!(
    /// A CI check's state, in GitHub's terms, which name the most; the other
    /// forges map their states onto these.
    CheckState: Variants {
        Pending => "pending",
        Queued => "queued",
        InProgress => "in_progress",
        /// Held for a runner to take it, or on GitHub for a deployment's
        /// approval.
        Waiting => "waiting",
        Requested => "requested",
        /// A required commit status no one has reported yet, as GitHub's
        /// GraphQL API reported it.
        Expected => "expected",
        /// Finished without a conclusion.
        Completed => "completed",
        Success => "success",
        Neutral => "neutral",
        Skipped => "skipped",
        /// Failed in a way the forge lets pass.
        Warning => "warning",
        Failure => "failure",
        Error => "error",
        TimedOut => "timed_out",
        StartupFailure => "startup_failure",
        Cancelled => "cancelled",
        ActionRequired => "action_required",
        Stale => "stale",
    }
);

impl CheckState {
    /// The state `text` names in any casing.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|state| state.as_str().eq_ignore_ascii_case(text))
    }

    /// Whether the check finished.
    pub fn complete(self) -> bool {
        !matches!(
            self,
            Self::Pending
                | Self::Queued
                | Self::InProgress
                | Self::Waiting
                | Self::Requested
                | Self::Expected
        )
    }

    /// Whether the check ran and failed, so its job may have a log worth
    /// reading; a cancelled check was stopped rather than failing.
    pub fn failed(self) -> bool {
        matches!(
            self,
            Self::Failure | Self::Error | Self::TimedOut | Self::StartupFailure
        )
    }

    /// Whether the check finished and passed; any other finished state,
    /// such as a cancelled one, may need attention.
    pub fn passed(self) -> bool {
        matches!(
            self,
            Self::Success | Self::Neutral | Self::Skipped | Self::Warning
        )
    }
}

/// A saved check's state, in any casing: saved snapshots spell states as
/// `Success` or `SUCCESS`.
pub(crate) fn saved<'de, D: Deserializer<'de>>(deserializer: D) -> Result<CheckState, D::Error> {
    let text = String::deserialize(deserializer)?;
    CheckState::parse(&text)
        .ok_or_else(|| serde::de::Error::custom(format!("unknown CI check state {text}")))
}

#[cfg(test)]
mod tests;
