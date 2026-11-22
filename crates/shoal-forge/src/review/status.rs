//! A PR's current state, CI checks and reviews, looked up on request.
use std::collections::BTreeMap;

use anyhow::{Result, bail};

use crate::{
    backends::Activity, ci::approval::ApprovalRequests, transport::account::Account,
    updates::Snapshot,
};
use shoal_forge_types::{
    ForgeRepo,
    review::status::{PrStatus, ReviewState},
};

/// Recording what a PR's status lookups found.
#[shoal_forge_derive::extension(pub(crate) StatusRecords)]
impl PrStatus {
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
        self.head = snapshot.head;
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

/// Reading a PR's status.
#[shoal_forge_derive::extension(pub StatusRequests, pub(crate) StatusSteps)]
impl ForgeRepo {
    /// Each failed lookup is recorded on the result instead of failing it.
    pub async fn pr_status(
        &self,
        account: &Account,
        pull: &shoal_forge_types::items::reference::ItemRef,
    ) -> PrStatus {
        let result = async {
            pull.belongs_to(self)?;
            let number = pull.pull()?;
            self.kind().pr_status(self, account, number, pull).await
        }
        .await;
        let mut status = result.unwrap_or_else(|error| PrStatus::failed(pull.clone(), &error));
        self.record_waiting_runs(account, &mut status).await;
        status
    }

    /// The runs on the open PR's head that wait for approval, or why they
    /// could not be read.
    async fn record_waiting_runs(&self, account: &Account, status: &mut PrStatus) {
        let Some(head) = &status.head else {
            return;
        };
        match self.waiting_runs(account, head.as_str()).await {
            Ok(runs) => status.waiting_runs = runs,
            Err(error) => status
                .errors
                .push(format!("waiting CI runs lookup failed: {error:#}")),
        }
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

/// Reviewers and verdicts from GitHub's reviews.
pub(crate) fn github_reviews(reviews: &[serde_json::Value]) -> Result<Vec<(String, Verdict)>> {
    let mut verdicts = Vec::new();
    for review in reviews {
        let verdict = match review["state"].as_str() {
            Some("APPROVED") => Verdict::Approved,
            Some("CHANGES_REQUESTED") => Verdict::ChangesRequested,
            Some("COMMENTED") => Verdict::Commented,
            Some("PENDING" | "DISMISSED") => continue,
            _ => bail!("unknown PR review state"),
        };
        let reviewer = review["user"]["login"].as_str().unwrap_or_default();
        verdicts.push((reviewer.to_owned(), verdict));
    }
    Ok(verdicts)
}

#[cfg(test)]
mod tests;
