//! Local reviews: a one-shot, read-only reviewer's findings on a workspace's
//! committed HEAD or a PR, which the daemon stores and the workspace's agent
//! answers with typed outcomes.
use std::{fmt, str::FromStr};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::state::states;

mod failure;
pub mod output;

states!(
    /// Where a review is in its run.
    ReviewState {
        /// Waiting to start; it takes the workspace's HEAD when it does.
        Queued => "queued",
        Running => "running",
        /// The reviewer returned its findings, whatever its verdict.
        Finished => "finished",
        /// No reviewer returned findings; the review says why.
        Failed => "failed",
        Cancelled => "cancelled",
    }
);

impl ReviewState {
    /// Whether the review has yet to end.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

states!(
    /// How much a finding matters, as the reviewer judged it.
    Severity: ValueEnum {
        /// Must change before merging.
        Blocker => "blocker",
        ShouldFix => "should-fix",
        Nit => "nit",
    }
);

states!(
    /// The reviewer's overall judgement, which is shown but decides nothing.
    Verdict {
        Approve => "approve",
        Comment => "comment",
        RequestChanges => "request_changes",
    }
);

states!(
    /// Why no reviewer returned findings.
    FailureReason {
        UsageLimit => "usage-limit",
        QuotaExhausted => "quota-exhausted",
        RateLimit => "rate-limit",
        AuthenticationFailed => "authentication-failed",
        /// The reviewer exited unsuccessfully for another reason.
        ExecutionFailed => "execution-failed",
        /// The reviewer's output is missing or does not match the schema.
        InvalidOutput => "invalid-output",
        TimedOut => "timed-out",
    }
);

impl FailureReason {
    /// What the reason means for the user.
    pub fn describe(self) -> &'static str {
        match self {
            Self::UsageLimit => "the reviewer's usage limit was reached; retry after it resets",
            Self::QuotaExhausted => "the reviewer's quota or credits are exhausted",
            Self::RateLimit => "the reviewer was rate limited; retry later",
            Self::AuthenticationFailed => "the reviewer could not authenticate; sign in again",
            Self::ExecutionFailed => "the reviewer exited unsuccessfully",
            Self::InvalidOutput => "the reviewer's output did not match the review schema",
            Self::TimedOut => "the reviewer ran longer than 30 minutes and was stopped",
        }
    }
}

states!(
    /// What a later review found of a finding resolved `fixed`.
    FixCheck {
        Confirmed => "confirmed",
        StillPresent => "still-present",
    }
);

/// A finding by its review and its number there, written `<review>.<n>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FindingId {
    pub review: i64,
    pub number: i64,
}

impl fmt::Display for FindingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.review, self.number)
    }
}

impl FromStr for FindingId {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let parse = || {
            let (review, number) = text.split_once('.')?;
            let id = Self {
                review: review.parse().ok()?,
                number: number.parse().ok()?,
            };
            (id.review > 0 && id.number > 0).then_some(id)
        };
        parse().with_context(|| {
            format!("{text:?} is not a finding; findings are <review>.<n>, such as 17.2")
        })
    }
}

impl Serialize for FindingId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for FindingId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// How the workspace's agent answered a finding; each outcome carries what
/// it needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Outcome {
    /// The code was changed to address it; the next review checks it.
    Fixed,
    /// The claim is wrong.
    FalsePositive { reason: String },
    /// The claim holds, but the code is not changed.
    WontFix { reason: String },
    /// The claim holds and other work fixes it.
    Deferred { issue: String },
    /// It makes the same point as another finding.
    Duplicate { of: FindingId },
}

states!(
    /// An outcome's name, as `shoal review resolve` takes it.
    OutcomeKind: ValueEnum {
        Fixed => "fixed",
        FalsePositive => "false-positive",
        WontFix => "wont-fix",
        Deferred => "deferred",
        Duplicate => "duplicate",
    }
);

impl Outcome {
    /// The outcome `kind` with its data, refusing data it does not take and
    /// missing data it needs.
    pub fn new(
        kind: OutcomeKind,
        reason: Option<String>,
        issue: Option<String>,
        of: Option<FindingId>,
    ) -> Result<Self> {
        let reason = reason.filter(|reason| !reason.trim().is_empty());
        let issue = issue.filter(|issue| !issue.trim().is_empty());
        let outcome = match (kind, reason, issue, of) {
            (OutcomeKind::Fixed, None, None, None) => Self::Fixed,
            (OutcomeKind::FalsePositive, Some(reason), None, None) => {
                Self::FalsePositive { reason }
            }
            (OutcomeKind::WontFix, Some(reason), None, None) => Self::WontFix { reason },
            (OutcomeKind::Deferred, None, Some(issue), None) => Self::Deferred { issue },
            (OutcomeKind::Duplicate, None, None, Some(of)) => Self::Duplicate { of },
            (kind, ..) => bail!("{kind} takes {}", kind.data()),
        };
        Ok(outcome)
    }

    pub fn kind(&self) -> OutcomeKind {
        match self {
            Self::Fixed => OutcomeKind::Fixed,
            Self::FalsePositive { .. } => OutcomeKind::FalsePositive,
            Self::WontFix { .. } => OutcomeKind::WontFix,
            Self::Deferred { .. } => OutcomeKind::Deferred,
            Self::Duplicate { .. } => OutcomeKind::Duplicate,
        }
    }

    /// The outcome with its data, as one line.
    pub fn describe(&self) -> String {
        match self {
            Self::Fixed => "fixed".into(),
            Self::FalsePositive { reason } => format!("false-positive: {reason}"),
            Self::WontFix { reason } => format!("wont-fix: {reason}"),
            Self::Deferred { issue } => format!("deferred to {issue}"),
            Self::Duplicate { of } => format!("duplicate of {of}"),
        }
    }
}

impl OutcomeKind {
    /// The option the outcome needs.
    fn data(self) -> &'static str {
        match self {
            Self::Fixed => "no --reason, --issue or --of",
            Self::FalsePositive | Self::WontFix => "--reason and nothing else",
            Self::Deferred => "--issue and nothing else",
            Self::Duplicate => "--of and nothing else",
        }
    }
}

/// What a review covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewTarget {
    /// The workspace's committed HEAD against its diff base.
    Workspace,
    /// A PR's head against its base, posted there when `post` is set.
    Pr { url: String, post: bool },
}

/// One reviewer finding and how it was answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub id: FindingId,
    pub severity: Severity,
    pub path: String,
    /// The line in the reviewed version of the file.
    pub line: u32,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<Reply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// What the review that checked a `fixed` outcome found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Check>,
}

impl Finding {
    pub fn resolved(&self) -> bool {
        self.outcome.is_some()
    }
}

/// A discussion reply to a finding, which does not resolve it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub created_at: i64,
    pub body: String,
}

/// A later review's check of a finding resolved `fixed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub result: FixCheck,
    /// The review that checked it.
    pub review: i64,
}

/// One review: what it covers, who ran it, how it ended and its findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub id: i64,
    pub workspace_id: String,
    /// The workspace's name.
    pub workspace: String,
    pub target: ReviewTarget,
    /// The agent asked to review.
    pub agent: String,
    /// The agent that produced the result, which differs from `agent` after
    /// a fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    pub state: ReviewState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureReason>,
    /// The commit reviewed, set when the review starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The commit it was compared against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Unix seconds.
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    /// Where a PR review was posted, or why it could not be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posted: Option<Posted>,
    /// Whether the workspace's HEAD has moved past the reviewed commit;
    /// absent when HEAD was not read. Never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outdated: Option<bool>,
}

/// What posting a PR review did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "posted", rename_all = "snake_case", deny_unknown_fields)]
pub enum Posted {
    /// The review is on the PR.
    Posted { url: String },
    /// The forge refused it or could not be reached.
    Failed { error: String },
}

impl Review {
    /// Seconds from start to end, once both are known.
    pub fn duration(&self) -> Option<i64> {
        Some(self.ended_at? - self.started_at?)
    }

    /// The findings no outcome resolves.
    pub fn unresolved(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|finding| !finding.resolved())
    }

    /// The finding `id`, if this review has it.
    pub fn finding(&self, id: FindingId) -> Option<&Finding> {
        self.findings.iter().find(|finding| finding.id == id)
    }

    /// The review's state in one line: queued, running, the finding counts
    /// by severity, or why it failed or that it was cancelled.
    pub fn status(&self) -> String {
        match self.state {
            ReviewState::Finished => {
                let counts = severity_counts(&self.findings);
                match counts.is_empty() {
                    true => "finished: no findings".into(),
                    false => format!("finished: {counts}"),
                }
            }
            ReviewState::Failed => match self.failure {
                Some(reason) => format!("failed: {reason}"),
                None => "failed".into(),
            },
            state => state.to_string(),
        }
    }
}

/// The findings counted by severity, most severe first as [`Severity::ALL`]
/// lists them, such as
/// "1 blocker, 2 should-fix"; empty without findings.
pub fn severity_counts(findings: &[Finding]) -> String {
    Severity::ALL
        .iter()
        .filter_map(|severity| {
            let count = findings
                .iter()
                .filter(|finding| finding.severity == *severity)
                .count();
            (count > 0).then(|| format!("{count} {severity}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The note every delivered or posted review with findings ends with: the
/// reviewer is automated, and its findings are claims to check.
pub const AUTOMATED_NOTE: &str = "This review is automated and can be wrong. Each finding is a \
     suggestion, not an instruction: check it against the code and the project's rules, and \
     reject it when it does not hold.";

#[cfg(test)]
mod tests;
