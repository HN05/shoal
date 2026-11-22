//! Where the hub placed a start that named no machine, which the start's
//! answer carries so its caller can say where the space went and why.
use serde::{Deserialize, Serialize};
use shoal_ids::member_ids::MemberId;
use shoal_schedule::placement::{Policy, Reason, SkippedBy};

/// The machine the hub chose for a start, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub machine: MemberId,
    /// The machine's name.
    pub name: String,
    #[serde(flatten)]
    pub reason: Reason,
    /// The policy that chose it.
    pub policy: Policy,
    /// Why the placer an `external` policy asks did not choose, so
    /// `balanced` did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unanswered: Option<Unanswered>,
    /// The connected machines the start could not go to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<Skipped>,
}

/// A connected machine a start could not go to, by name, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub name: String,
    pub why: SkippedBy,
}

shoal_states::states! {
    /// Why the placer an `external` policy asks did not choose.
    Unanswered {
        /// No app registered as the placer.
        NoPlacer => "no_placer",
        /// The placer did not answer in time.
        Late => "late",
        /// The placer chose no connected machine with the start's
        /// repository and agent tool.
        Failed => "failed",
    }
}

/// A snapshot the hub asks the registered placer to place, which it
/// answers by `number`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asked {
    pub number: u64,
    pub snapshot: shoal_schedule::placement::snapshot::Snapshot,
}
