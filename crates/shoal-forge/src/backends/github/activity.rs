//! GitHub's item activity, read through its REST API, which answers a
//! repeated read as unchanged.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::GitHub;
use crate::{
    activity::{
        Activity,
        status::{StatusRecords, Statuses, github_reviews},
    },
    backends::rest::{fingerprints, read_pull},
    items::{
        discussion::{Views, login},
        item::ItemRequests,
    },
    transport::{
        account::Account,
        api::{Request, TransportRequests},
        backend::LoginHint,
    },
    updates::{Check, DISCUSSION, Entry, Snapshot},
};
use shoal_forge_types::{
    ForgeRepo,
    activity::{
        status::PrStatus,
        view::{Details, Review},
    },
    ci::check_state::CheckState,
    items::{
        item::{Current, ItemState},
        link::ItemKind,
    },
    numbers::{IssueNumber, PullNumber},
};

mod view;

impl Activity for GitHub {
    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Snapshot> {
        issue_activity(repo, account, number).await.hint_login(self)
    }

    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Snapshot> {
        pr_activity(repo, account, number).await
    }
}

impl Statuses for GitHub {
    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        pull: &shoal_forge_types::items::reference::ItemRef,
    ) -> Result<PrStatus> {
        pr_status(repo, account, number, pull)
            .await
            .hint_login(self)
    }
}

impl Views for GitHub {
    async fn view_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comments: bool,
    ) -> Result<Details> {
        view::details(repo, account, kind, number, comments).await
    }

    /// The comments are read with the item; this reads the reviews.
    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        _details: &mut Details,
        _errors: &mut Vec<String>,
    ) -> Result<Option<Vec<Review>>> {
        view::reviews(repo, account, kind, number).await
    }
}

async fn issue_activity(
    repo: &ForgeRepo,
    account: &Account,
    number: IssueNumber,
) -> Result<Snapshot> {
    let (issue, discussion) = tokio::join!(
        repo.read(account, ItemKind::Issue, number.get()),
        discussion(repo, account, number.get()),
    );
    let mut snapshot = Snapshot::item(&issue?);
    snapshot.record_comments(DISCUSSION, discussion);
    Ok(snapshot)
}

async fn pr_activity(repo: &ForgeRepo, account: &Account, number: PullNumber) -> Result<Snapshot> {
    let pull = read_pull(repo, account, number).await?;
    let mut snapshot = Snapshot::item(&pull);
    let (discussion, reviews, inline, checks) = tokio::join!(
        discussion(repo, account, number.get()),
        reviews(repo, account, number),
        inline_comments(repo, account, number),
        open_checks(repo, account, &pull),
    );
    snapshot.record_comments(DISCUSSION, discussion);
    snapshot.record_comments("reviews", reviews.and_then(|r| review_entries(&r)));
    snapshot.record_comments("inline", inline);
    if let Some(checks) = checks {
        snapshot.record_keyed_checks(&pull, checks);
    }
    Ok(snapshot)
}

async fn pr_status(
    repo: &ForgeRepo,
    account: &Account,
    number: PullNumber,
    pull_ref: &shoal_forge_types::items::reference::ItemRef,
) -> Result<PrStatus> {
    let pull = repo.read(account, ItemKind::Pr, number.get()).await?;
    let (checks, reviews) = tokio::join!(
        open_checks(repo, account, &pull),
        reviews(repo, account, number),
    );
    let mut status = PrStatus::new(pull_ref.clone(), Some(pull.item.state));
    if let Some(checks) = checks {
        let mut snapshot = Snapshot::default();
        snapshot.record_keyed_checks(&pull, checks);
        status.record_snapshot(snapshot);
    }
    status.record_reviews(reviews.and_then(|reviews| github_reviews(&reviews)));
    Ok(status)
}

/// The checks on `pull`'s head while it is open; a closed PR has none to read.
async fn open_checks(
    repo: &ForgeRepo,
    account: &Account,
    pull: &Current,
) -> Option<Result<Vec<(String, Check)>>> {
    if pull.item.state != ItemState::Open {
        return None;
    }
    Some(checks(repo, account, pull).await)
}

/// The item's discussion comments.
async fn discussion(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<Vec<(String, Entry)>> {
    let endpoint = repo.repo_endpoint(&format!("issues/{number}/comments"));
    comment_entries(&repo.pages(account, &endpoint).await?)
}

async fn reviews(repo: &ForgeRepo, account: &Account, number: PullNumber) -> Result<Vec<Value>> {
    let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews"));
    repo.pages(account, &endpoint).await
}

async fn inline_comments(
    repo: &ForgeRepo,
    account: &Account,
    number: PullNumber,
) -> Result<Vec<(String, Entry)>> {
    let endpoint = repo.repo_endpoint(&format!("pulls/{number}/comments"));
    comment_entries(&repo.pages(account, &endpoint).await?)
}

/// The check runs and commit statuses on the PR's head commit.
async fn checks(
    repo: &ForgeRepo,
    account: &Account,
    pull: &Current,
) -> Result<Vec<(String, Check)>> {
    let head = pull
        .head_sha
        .as_ref()
        .map(shoal_refs::CommitId::as_str)
        .context("PR has no head commit")?;
    let (runs, statuses) = commit_checks(repo, account, head).await?;
    let mut checks = check_runs(head, &runs)?;
    checks.extend(commit_statuses(head, &statuses)?);
    Ok(checks)
}

/// The latest check runs and the combined status of commit `head`.
pub(super) async fn commit_checks(
    repo: &ForgeRepo,
    account: &Account,
    head: &str,
) -> Result<(Value, Value)> {
    let runs = Request::get(repo.repo_endpoint(&format!(
        "commits/{head}/check-runs?filter=latest&per_page=100"
    )));
    let statuses = Request::get(repo.repo_endpoint(&format!("commits/{head}/status?per_page=100")));
    tokio::try_join!(repo.send(account, &runs), repo.send(account, &statuses))
}

fn check_runs(head: &str, response: &Value) -> Result<Vec<(String, Check)>> {
    let runs = response["check_runs"]
        .as_array()
        .context("GitHub check runs are not a list")?;
    ensure!(
        response["total_count"].as_u64() == Some(runs.len() as u64),
        "GitHub returned a partial page of check runs"
    );
    runs.iter()
        .map(|run| {
            let status = run["status"].as_str().context("check run has no status")?;
            let state = run["conclusion"]
                .as_str()
                .filter(|conclusion| !conclusion.is_empty())
                .unwrap_or(status);
            let revision = revision(head, &run["completed_at"], &run["started_at"]);
            let check = rolled_up(&run["name"], state, revision)?;
            Ok((check_key(&run["details_url"], &check), check))
        })
        .collect()
}

fn commit_statuses(head: &str, combined: &Value) -> Result<Vec<(String, Check)>> {
    let statuses = combined["statuses"]
        .as_array()
        .context("GitHub commit statuses are not a list")?;
    ensure!(
        combined["total_count"].as_u64() == Some(statuses.len() as u64),
        "GitHub returned a partial page of commit statuses"
    );
    statuses
        .iter()
        .map(|status| {
            let state = status["state"]
                .as_str()
                .context("commit status has no state")?;
            let revision = revision(head, &Value::Null, &status["created_at"]);
            let check = rolled_up(&status["context"], state, revision)?;
            Ok((check_key(&status["target_url"], &check), check))
        })
        .collect()
}

/// A check named `name` in `state`.
fn rolled_up(name: &Value, state: &str, revision: String) -> Result<Check> {
    let name = name
        .as_str()
        .filter(|name| !name.is_empty())
        .context("CI check has no name")?;
    Ok(Check {
        name: name.into(),
        state: CheckState::parse(state)
            .with_context(|| format!("unknown GitHub check state {state}"))?,
        revision,
    })
}

/// The key of `check`, whose details are at `url`.
fn check_key(url: &Value, check: &Check) -> String {
    format!("{}:{}", url.as_str().unwrap_or_default(), check.name)
}

/// A check's run on `head` that ended at `completed` and started at
/// `started`, so a rerun on the same commit reads as another run.
fn revision(head: &str, completed: &Value, started: &Value) -> String {
    format!("{}:{completed}:{started}", Value::from(head))
}

/// Each comment by its ID, which changes when it is edited. Its
/// `updated_at` stays out: a reaction moves it too.
fn comment_entries(comments: &[Value]) -> Result<Vec<(String, Entry)>> {
    fingerprints(comments, &["body"], |comment| login(&comment["user"]))
}

/// Each review by its ID, which changes when it is edited or dismissed.
fn review_entries(reviews: &[Value]) -> Result<Vec<(String, Entry)>> {
    fingerprints(reviews, &["body", "state"], |review| login(&review["user"]))
}

#[cfg(test)]
mod tests;
