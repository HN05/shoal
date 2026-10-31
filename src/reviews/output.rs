//! What a reviewer returns: the output schema it is given and the result
//! read back from it.
use serde::{Deserialize, Serialize};

use super::{FindingId, FixCheck, Severity, Verdict};

/// A reviewer's result, as the output schema describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerOutput {
    pub summary: String,
    pub verdict: Verdict,
    pub findings: Vec<NewFinding>,
    /// What it found of each earlier finding resolved `fixed`.
    pub checked: Vec<Checked>,
}

/// A finding as the reviewer reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewFinding {
    pub path: String,
    pub line: u32,
    pub severity: Severity,
    pub title: String,
    pub body: String,
}

/// The reviewer's check of an earlier finding resolved `fixed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checked {
    pub finding: FindingId,
    pub result: FixCheck,
}
