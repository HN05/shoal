//! A PR's current state, CI checks and reviews, looked up on request.
use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{ForgeRepo, account::Account, backends::Backend, updates::Snapshot};

crate::state::states!(PrState {
    Open => "open",
    Merged => "merged",
    Closed => "closed",
});

crate::state::states!(ReviewState {
    Unreviewed => "none",
    Commented => "commented",
    Approved => "approved",
    ChangesRequested => "changes_requested",
});

/// A failed lookup leaves the fields it would have filled empty and records
/// its error instead of failing the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrStatus {
    pub url: String,
    pub state: Option<PrState>,
    /// Unknown while the forge computes mergeability, and for drafts and closed PRs.
    pub merge_conflicts: Option<bool>,
    pub checks: Vec<CheckResult>,
    pub review: Option<ReviewState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    /// The forge's result, lowercased, such as `success` or `in_progress`.
    pub result: String,
}

impl CheckResult {
    /// Whether the check ran and failed, so its job may have a log worth
    /// reading; a cancelled check was stopped rather than failing.
    pub fn failed(&self) -> bool {
        matches!(
            self.result.as_str(),
            "failure" | "error" | "timed_out" | "startup_failure"
        )
    }

    /// Whether the check finished and passed; any other finished result,
    /// such as a cancelled one, may need attention.
    pub fn passed(&self) -> bool {
        matches!(
            self.result.as_str(),
            "success" | "neutral" | "skipped" | "warning"
        )
    }
}

impl PrStatus {
    pub fn new(url: String, state: Option<PrState>) -> Self {
        Self {
            url,
            state,
            merge_conflicts: None,
            checks: Vec::new(),
            review: None,
            errors: Vec::new(),
        }
    }

    pub fn failed(url: String, error: &anyhow::Error) -> Self {
        Self {
            errors: vec![format!("{error:#}")],
            ..Self::new(url, None)
        }
    }

    /// An open PR's mergeability and checks as `snapshot` observed them,
    /// with the lookups that failed.
    pub(crate) fn record_snapshot(&mut self, snapshot: Snapshot) {
        self.errors.extend(
            snapshot
                .errors
                .iter()
                .map(|(source, error)| format!("{source} lookup failed: {error}")),
        );
        // A status reports the checks it could not read, whatever stopped it.
        if let Some(limit) = &snapshot.limited {
            self.errors.push(format!("CI lookup failed: {limit}"));
        }
        self.merge_conflicts = snapshot.conflict;
        self.checks = snapshot.check_results();
    }

    /// The review state, or why it could not be read.
    pub(crate) fn record_reviews(&mut self, reviews: Result<Vec<(String, Verdict)>>) {
        match reviews {
            Ok(reviews) => self.review = Some(summarize(reviews)),
            Err(error) => self
                .errors
                .push(format!("reviews lookup failed: {error:#}")),
        }
    }
}

impl ForgeRepo {
    /// Each failed lookup is recorded on the result instead of failing it.
    pub async fn pr_status(&self, account: &Account, url: &str) -> PrStatus {
        let result = async {
            let (number, _) = self.pull(url)?;
            self.kind.pr_status(self, account, number, url).await
        }
        .await;
        result.unwrap_or_else(|error| PrStatus::failed(url.into(), &error))
    }
}

/// One submitted review; pending drafts are not reviews yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Approved,
    ChangesRequested,
    Commented,
}

/// Each reviewer's latest approval or change request counts; a later comment
/// does not withdraw it. Any change request outweighs approvals.
pub(crate) fn summarize(reviews: impl IntoIterator<Item = (String, Verdict)>) -> ReviewState {
    let mut latest = BTreeMap::new();
    for (reviewer, verdict) in reviews {
        let entry = latest.entry(reviewer).or_insert(verdict);
        if verdict != Verdict::Commented {
            *entry = verdict;
        }
    }
    let verdicts = latest.into_values().collect::<Vec<_>>();
    if verdicts.contains(&Verdict::ChangesRequested) {
        ReviewState::ChangesRequested
    } else if verdicts.contains(&Verdict::Approved) {
        ReviewState::Approved
    } else if verdicts.is_empty() {
        ReviewState::Unreviewed
    } else {
        ReviewState::Commented
    }
}

/// Reviewers and verdicts from Forgejo's reviews, leaving out stale and
/// dismissed ones, which no longer describe the PR.
pub(crate) fn forgejo_reviews(reviews: &[serde_json::Value]) -> Result<Vec<(String, Verdict)>> {
    let mut verdicts = Vec::new();
    for review in reviews {
        let verdict = match review["state"].as_str() {
            Some("APPROVED") => Verdict::Approved,
            Some("REQUEST_CHANGES") => Verdict::ChangesRequested,
            Some("COMMENT") => Verdict::Commented,
            Some("PENDING" | "REQUEST_REVIEW") => continue,
            _ => bail!("unknown Forgejo review state"),
        };
        if review["stale"] == true || review["dismissed"] == true {
            continue;
        }
        let reviewer = review["user"]["login"].as_str().unwrap_or_default();
        verdicts.push((reviewer.to_owned(), verdict));
    }
    Ok(verdicts)
}

/// Reviewers and verdicts from `gh pr view --json reviews`.
pub(crate) fn github_reviews(pull: &serde_json::Value) -> Result<Vec<(String, Verdict)>> {
    let Some(reviews) = pull["reviews"].as_array() else {
        bail!("missing PR reviews");
    };
    let mut verdicts = Vec::new();
    for review in reviews {
        let verdict = match review["state"].as_str() {
            Some("APPROVED") => Verdict::Approved,
            Some("CHANGES_REQUESTED") => Verdict::ChangesRequested,
            Some("COMMENTED") => Verdict::Commented,
            Some("PENDING" | "DISMISSED") => continue,
            _ => bail!("unknown PR review state"),
        };
        let reviewer = review["author"]["login"].as_str().unwrap_or_default();
        verdicts.push((reviewer.to_owned(), verdict));
    }
    Ok(verdicts)
}

#[cfg(test)]
mod tests;
