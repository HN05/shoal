//! Local reviews: a one-shot, read-only reviewer's run over a workspace's
//! committed HEAD or a PR, whose findings are `review` threads the
//! workspace's agent resolves, or a review the user or a space writes
//! itself, whose comments are `review` threads its writer resolves.
use shoal_ids::space_ids::SpaceId;
use std::{fmt, str::FromStr};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use shoal_refs::{BranchName, CommitId};
use shoal_states::states;

use crate::space_names::SpaceName;
use crate::threads::{Thread, ThreadSpace};

states!(
    /// Where a review is in its run.
    ReviewState {
        /// Waiting to start; it takes the workspace's HEAD when it does.
        Queued => "queued",
        Running => "running",
        /// Being written by the user or a space, whose comments reach
        /// nobody until it is submitted.
        Pending => "pending",
        /// The reviewer returned its findings, or its writer submitted it,
        /// whatever its verdict.
        Finished => "finished",
        /// No reviewer returned findings; the review says why.
        Failed => "failed",
        Cancelled => "cancelled",
    }
);

impl ReviewState {
    /// Whether the review has yet to end.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Pending)
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
        /// The repository's false positives file cannot be read.
        InvalidFalsePositives => "invalid-false-positives",
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
            Self::InvalidFalsePositives => {
                "the repository's .shoal/false-positives.toml is invalid; fix or delete it, then review again"
            }
        }
    }
}

/// A review's ID, which the daemon numbers from 1 and the protocol and the
/// store carry as an integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReviewId(pub i64);

impl fmt::Display for ReviewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for ReviewId {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        text.parse()
            .ok()
            .filter(|id| *id > 0)
            .map(Self)
            .with_context(|| format!("{text:?} is not a review ID; reviews are numbers"))
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::ToSql for ReviewId {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::FromSql for ReviewId {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        i64::column_result(value).map(Self)
    }
}

/// A review queued or running in a workspace as a package of it was
/// written, which the machine that imports the package queues again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedReview {
    pub target: ReviewTarget,
    /// The agent asked to review.
    pub agent: crate::agent::Agent,
    #[serde(default, skip_serializing_if = "crate::agent::ModelChoice::is_empty")]
    pub model: crate::agent::ModelChoice,
}

/// What a review covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewTarget {
    /// The workspace's committed HEAD against its diff base.
    Workspace,
    /// A PR's head branch against its base branch, both as fetched from
    /// origin, posted there when `post` is set.
    Pr {
        url: crate::forge::items::reference::ItemRef,
        head: BranchName,
        base: BranchName,
        post: bool,
    },
}

/// Who writes a review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum Author {
    /// Shoal, whose reviewer is a one-shot run of `agent`, with `model`
    /// over the agent's configured model and effort.
    Shoal {
        agent: crate::agent::Agent,
        #[serde(default, skip_serializing_if = "crate::agent::ModelChoice::is_empty")]
        model: crate::agent::ModelChoice,
    },
    /// The user, who writes it.
    User,
    /// A space's agent, such as a parent's, which writes it.
    Space { space: ThreadSpace },
}

impl Author {
    /// Who wrote it, as text names them: the agent Shoal's reviewer runs,
    /// the user or the space.
    pub fn label(&self) -> String {
        match self {
            Self::Shoal { agent, .. } => agent.to_string(),
            Self::User => "the user".into(),
            Self::Space { space } => space.label(),
        }
    }
}

/// One review: what it covers, who wrote it, how it ended, and the threads
/// of its findings or comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub id: ReviewId,
    pub workspace_id: SpaceId,
    /// The workspace's name.
    pub workspace: SpaceName,
    pub target: ReviewTarget,
    pub author: Author,
    /// The agent that produced the result of Shoal's reviewer, which
    /// differs from the one asked for after a fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<crate::agent::Agent>,
    pub state: ReviewState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureReason>,
    /// The commit reviewed, set when the review starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<CommitId>,
    /// The commit it was compared against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<CommitId>,
    pub created_at: crate::time::UnixTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<crate::time::UnixTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<crate::time::UnixTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// The `review` thread it opened for each finding, in the reviewer's
    /// order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<Thread>,
    /// The earlier findings resolved `fixed` that it found still present
    /// and reopened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reopened: Vec<Thread>,
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
    /// The agent Shoal's reviewer runs, none for a review the user or a
    /// space writes.
    pub fn agent(&self) -> Option<&crate::agent::Agent> {
        match &self.author {
            Author::Shoal { agent, .. } => Some(agent),
            Author::User | Author::Space { .. } => None,
        }
    }

    /// The model and effort asked for the agent Shoal's reviewer runs.
    pub fn model(&self) -> Option<&crate::agent::ModelChoice> {
        match &self.author {
            Author::Shoal { model, .. } => Some(model),
            Author::User | Author::Space { .. } => None,
        }
    }

    /// Who reviewed, as text names them: the agent that produced the result
    /// of Shoal's reviewer, else whoever wrote the review.
    pub fn reviewed_by(&self) -> String {
        match &self.reviewer {
            Some(reviewer) => reviewer.to_string(),
            None => self.author.label(),
        }
    }

    /// The time from start to end, once both are known.
    pub fn duration(&self) -> Option<std::time::Duration> {
        Some(self.ended_at?.duration_since(self.started_at?))
    }

    /// The threads it opened or reopened that are open.
    pub fn unresolved(&self) -> impl Iterator<Item = &Thread> {
        (self.threads.iter().chain(&self.reopened)).filter(|thread| thread.is_open())
    }

    /// The review's state in one line: queued, running, pending with its
    /// comments so far, the finding counts by severity and how many it
    /// reopened, or why it failed or that it was cancelled.
    pub fn status(&self) -> String {
        match self.state {
            ReviewState::Finished => {
                let counts = severity_counts(&self.threads);
                let reopened = match self.reopened.len() {
                    0 => String::new(),
                    count => format!("{count} reopened"),
                };
                let found: Vec<String> = [counts, reopened]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect();
                match found.is_empty() {
                    true => "finished: no findings".into(),
                    false => format!("finished: {}", found.join(", ")),
                }
            }
            ReviewState::Pending => match severity_counts(&self.threads) {
                counts if counts.is_empty() => "pending".into(),
                counts => format!("pending: {counts}"),
            },
            ReviewState::Failed => match self.failure {
                Some(reason) => format!("failed: {reason}"),
                None => "failed".into(),
            },
            state => state.to_string(),
        }
    }
}

/// A workspace's latest review as lists show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewStatus {
    pub id: ReviewId,
    pub state: ReviewState,
    /// [`Review::status`].
    pub status: String,
    pub unresolved: usize,
}

impl From<&Review> for ReviewStatus {
    fn from(review: &Review) -> Self {
        Self {
            id: review.id,
            state: review.state,
            status: review.status(),
            unresolved: review.unresolved().count(),
        }
    }
}

/// The findings of `threads` counted by severity, most severe first as
/// [`Severity::ALL`] lists them, then the comments without one, such as
/// "1 blocker, 2 should-fix" or "3 comments"; empty without any.
pub fn severity_counts(threads: &[Thread]) -> String {
    let count = |severity: Option<Severity>| {
        (threads.iter())
            .filter(|thread| thread.finding.is_some_and(|f| f.severity == severity))
            .count()
    };
    let findings = (Severity::ALL.iter()).filter_map(|severity| {
        let count = count(Some(*severity));
        (count > 0).then(|| format!("{count} {severity}"))
    });
    let comments = match count(None) {
        0 => None,
        1 => Some("1 comment".to_owned()),
        count => Some(format!("{count} comments")),
    };
    findings.chain(comments).collect::<Vec<_>>().join(", ")
}

/// The note every delivered or posted review with findings ends with: the
/// reviewer is automated, and its findings are claims to check.
pub const AUTOMATED_NOTE: &str = "This review is automated and can be wrong. Each finding is a \
     suggestion, not an instruction: check it against the code and the project's rules, and \
     reject it when it does not hold.";

#[cfg(test)]
mod tests;
