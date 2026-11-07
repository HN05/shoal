//! The state of a CI check, which every forge's checks and commit statuses
//! map onto.
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A CI check's state, in GitHub's terms, which name the most; the other
/// forges map their states onto these. Serialized in lower case, and read
/// in any casing, since saved snapshots spell states as `Success` or
/// `SUCCESS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Pending,
    Queued,
    InProgress,
    /// Held for a runner to take it, or on GitHub for a deployment's
    /// approval.
    Waiting,
    Requested,
    /// Finished without a conclusion.
    Completed,
    Success,
    Neutral,
    Skipped,
    /// Failed in a way the forge lets pass.
    Warning,
    Failure,
    Error,
    TimedOut,
    StartupFailure,
    Cancelled,
    ActionRequired,
    Stale,
}

impl CheckState {
    pub const ALL: &'static [Self] = &[
        Self::Pending,
        Self::Queued,
        Self::InProgress,
        Self::Waiting,
        Self::Requested,
        Self::Completed,
        Self::Success,
        Self::Neutral,
        Self::Skipped,
        Self::Warning,
        Self::Failure,
        Self::Error,
        Self::TimedOut,
        Self::StartupFailure,
        Self::Cancelled,
        Self::ActionRequired,
        Self::Stale,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Queued => "queued",
            Self::InProgress => "in_progress",
            Self::Waiting => "waiting",
            Self::Requested => "requested",
            Self::Completed => "completed",
            Self::Success => "success",
            Self::Neutral => "neutral",
            Self::Skipped => "skipped",
            Self::Warning => "warning",
            Self::Failure => "failure",
            Self::Error => "error",
            Self::TimedOut => "timed_out",
            Self::StartupFailure => "startup_failure",
            Self::Cancelled => "cancelled",
            Self::ActionRequired => "action_required",
            Self::Stale => "stale",
        }
    }

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
            Self::Pending | Self::Queued | Self::InProgress | Self::Waiting | Self::Requested
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

impl std::fmt::Display for CheckState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for CheckState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CheckState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown CI check state {text}")))
    }
}

#[cfg(test)]
mod tests;
