//! Forgejo's item activity, PR status and item views.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::{Forgejo, actions};
use crate::{
    ForgeRepo,
    account::Account,
    backends::{
        Activity, LoginHint,
        rest::{self, RestCheck, RestReads, fingerprints},
    },
    ci::check_state::CheckState,
    items::{item::Current, link::ItemKind},
    numbers::{IssueNumber, PullNumber},
    review::{
        status::{PrStatus, Verdict, forgejo_reviews},
        view::{Details, Review, login},
    },
    transport::api::Request,
    updates::{Entry, Snapshot},
};

mod view;

impl Activity for Forgejo {
    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Snapshot> {
        rest::issue_activity(self, repo, account, number).await
    }

    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Snapshot> {
        rest::pr_activity(self, repo, account, number).await
    }

    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        pull: &crate::items::reference::ItemRef,
    ) -> Result<PrStatus> {
        rest::pr_status(self, repo, account, number, pull).await
    }

    async fn view_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        _comments: bool,
    ) -> Result<Details> {
        view::details(repo, account, kind, number)
            .await
            .hint_login(self)
    }

    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        details: &mut Details,
        errors: &mut Vec<String>,
    ) -> Result<Option<Vec<Review>>> {
        view::discussion(repo, account, kind, number, details, errors)
            .await
            .hint_login(self)
    }
}

impl RestReads for Forgejo {
    async fn discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        _kind: ItemKind,
        number: u64,
    ) -> Result<Vec<(String, Entry)>> {
        let endpoint = repo.repo_endpoint(&format!("issues/{number}/comments"));
        let comments = repo.pages(account, &endpoint).await?;
        fingerprints(&comments, &["body", "updated_at"], |comment| {
            login(&comment["user"])
        })
    }

    async fn review_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Vec<(String, Entry)>> {
        let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews"));
        let mut reviews = repo.pages(account, &endpoint).await?;
        reviews.retain(submitted);
        fingerprints(
            &reviews,
            &[
                "body",
                "state",
                "comments_count",
                "stale",
                "dismissed",
                "updated_at",
            ],
            |review| login(&review["user"]),
        )
    }

    async fn checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        pull: &Current,
    ) -> Result<Vec<RestCheck>> {
        let head = pull
            .head_sha
            .as_ref()
            .map(shoal_refs::CommitId::as_str)
            .context("PR has no head commit")?;
        let endpoint = repo.repo_endpoint(&format!("commits/{head}/status?limit=50"));
        let combined = repo.send(account, &Request::get(endpoint)).await?;
        statuses(&combined)
    }

    async fn verdicts(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Vec<(String, Verdict)>> {
        let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews"));
        forgejo_reviews(&repo.pages(account, &endpoint).await?)
    }
}

/// Whether a Forgejo review record is a submitted review. Requesting a
/// reviewer adds a `REQUEST_REVIEW` record, and a draft is `PENDING`.
fn submitted(review: &Value) -> bool {
    !matches!(review["state"].as_str(), Some("PENDING" | "REQUEST_REVIEW"))
}

/// Forgejo's combined status; a commit without statuses has `"statuses": null`.
fn statuses(combined: &Value) -> Result<Vec<RestCheck>> {
    let statuses = match &combined["statuses"] {
        Value::Null => &[][..],
        statuses => statuses.as_array().context("invalid Forgejo CI statuses")?,
    };
    ensure!(
        combined["total_count"].as_u64() == Some(statuses.len() as u64),
        "Forgejo returned a partial page of CI statuses"
    );
    statuses
        .iter()
        .map(|status| {
            let name = status["context"]
                .as_str()
                .context("Forgejo CI status has no context")?;
            let state = match status["status"].as_str() {
                Some("pending") if actions::status_waits(status) => CheckState::Waiting,
                Some("pending") => CheckState::Pending,
                Some("success") => CheckState::Success,
                Some("failure") => CheckState::Failure,
                Some("warning") => CheckState::Warning,
                Some("skipped") => CheckState::Skipped,
                Some("error") => CheckState::Error,
                _ => anyhow::bail!("unknown Forgejo CI state"),
            };
            Ok(RestCheck {
                name: name.into(),
                state,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
