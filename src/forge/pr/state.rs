//! A PR's current state, CI checks and reviews, looked up on request.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{Registration, RegistrationKind};
use crate::{
    forge::{ForgeRepo, account::Account, repository},
    model::Workspace,
};

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
}

/// Look up each watched PR concurrently; a failure stays on its PR.
pub(crate) async fn watched(
    account: &Account,
    workspace: &Workspace,
    registration: Option<&Registration>,
) -> Vec<PrStatus> {
    let Some(Registration {
        kind: RegistrationKind::Watch { urls, .. },
        ..
    }) = registration
    else {
        return Vec::new();
    };
    let forge = async {
        let remote = repository::remote_url_from_path(&workspace.path)
            .await?
            .context("PR lookup needs an origin remote")?;
        ForgeRepo::parse(&remote)
    }
    .await;
    match forge {
        Ok(forge) => {
            futures_util::future::join_all(urls.iter().map(|url| forge.pr_status(account, url)))
                .await
        }
        Err(error) => urls
            .iter()
            .map(|url| PrStatus::failed(url.clone(), &error))
            .collect(),
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
mod tests {
    use super::*;

    fn review(reviewer: &str, verdict: Verdict) -> (String, Verdict) {
        (reviewer.into(), verdict)
    }

    #[test]
    fn latest_verdict_per_reviewer_decides_and_comments_do_not_withdraw_it() {
        use Verdict::*;
        assert_eq!(summarize([]), ReviewState::Unreviewed);
        assert_eq!(
            summarize([review("bot", Commented)]),
            ReviewState::Commented
        );
        assert_eq!(
            summarize([review("a", Approved), review("a", Commented)]),
            ReviewState::Approved
        );
        assert_eq!(
            summarize([review("a", ChangesRequested), review("a", Approved)]),
            ReviewState::Approved
        );
        assert_eq!(
            summarize([review("a", Approved), review("b", ChangesRequested)]),
            ReviewState::ChangesRequested
        );
    }

    #[test]
    fn forgejo_reviews_skip_pending_stale_and_dismissed_ones() {
        let reviews = serde_json::json!([
            {"user": {"login": "bot"}, "state": "COMMENT"},
            {"user": {"login": "reviewer"}, "state": "REQUEST_CHANGES"},
            {"user": {"login": "old"}, "state": "APPROVED", "stale": true},
            {"user": {"login": "gone"}, "state": "APPROVED", "dismissed": true},
            {"user": {"login": "me"}, "state": "PENDING"},
            {"user": {"login": "asked"}, "state": "REQUEST_REVIEW"}
        ]);
        assert_eq!(
            forgejo_reviews(reviews.as_array().unwrap()).unwrap(),
            [
                review("bot", Verdict::Commented),
                review("reviewer", Verdict::ChangesRequested)
            ]
        );
        assert!(forgejo_reviews(&[serde_json::json!({"state": "LATER"})]).is_err());
    }

    #[test]
    fn github_reviews_skip_drafts_and_dismissals() {
        let pull = serde_json::json!({"reviews": [
            {"author": {"login": "a"}, "state": "APPROVED"},
            {"author": {"login": "b"}, "state": "DISMISSED"},
            {"author": {"login": "c"}, "state": "PENDING"},
            {"author": {"login": "d"}, "state": "COMMENTED"},
        ]});
        assert_eq!(
            github_reviews(&pull).unwrap(),
            [
                review("a", Verdict::Approved),
                review("d", Verdict::Commented)
            ]
        );
        assert!(github_reviews(&serde_json::json!({})).is_err());
        assert!(github_reviews(&serde_json::json!({"reviews": [{"state": "LATER"}]})).is_err());
    }
}
