//! GitHub activity and PR status, read through its REST API, which answers
//! repeated reads as unchanged and reports its rate limit. Snapshots key,
//! fingerprint and name what they record as they did when `gh` read it
//! through GraphQL, so snapshots saved then read as unchanged.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::{
    ForgeRepo,
    account::Account,
    backends::rest::read_pull,
    ci::check_state::CheckState,
    items::{
        item::{Current, ItemState},
        link::ItemKind,
    },
    review::{
        status::{PrState, PrStatus, github_reviews},
        view::login,
    },
    transport::api::Request,
    updates::{Check, DISCUSSION, Entry, Snapshot, id_text},
};

pub(crate) async fn issue_activity(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<Snapshot> {
    let (issue, discussion) = tokio::join!(
        repo.read(account, ItemKind::Issue, number),
        discussion(repo, account, number),
    );
    let mut snapshot = Snapshot::item(&issue?);
    snapshot.record_comments(DISCUSSION, discussion);
    Ok(snapshot)
}

pub(crate) async fn pr_activity(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<Snapshot> {
    let pull = read_pull(repo, account, number).await?;
    let mut snapshot = Snapshot::item(&pull);
    let (discussion, reviews, inline, checks) = tokio::join!(
        discussion(repo, account, number),
        reviews(repo, account, number),
        inline_comments(repo, account, number),
        open_checks(repo, account, &pull),
    );
    snapshot.record_comments(DISCUSSION, discussion);
    snapshot.record_comments("reviews", reviews.and_then(|r| entries(&r, "node_id")));
    snapshot.record_comments("inline", inline);
    if let Some(checks) = checks {
        snapshot.record_keyed_checks(&pull, checks);
    }
    Ok(snapshot)
}

pub(crate) async fn pr_status(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    url: &str,
) -> Result<PrStatus> {
    let pull = repo.read(account, ItemKind::Pr, number).await?;
    let (checks, reviews) = tokio::join!(
        open_checks(repo, account, &pull),
        reviews(repo, account, number),
    );
    let mut status = PrStatus::new(url.into(), Some(PrState::from(pull.item.state)));
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

/// The item's discussion comments, by the GraphQL node IDs `gh` reported.
async fn discussion(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<Vec<(String, Entry)>> {
    let endpoint = repo.repo_endpoint(&format!("issues/{number}/comments"));
    entries(&repo.pages(account, &endpoint).await?, "node_id")
}

async fn reviews(repo: &ForgeRepo, account: &Account, number: u64) -> Result<Vec<Value>> {
    let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews"));
    repo.pages(account, &endpoint).await
}

/// The PR's inline comments, by their REST IDs.
async fn inline_comments(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<Vec<(String, Entry)>> {
    let endpoint = repo.repo_endpoint(&format!("pulls/{number}/comments"));
    entries(&repo.pages(account, &endpoint).await?, "id")
}

/// The check runs and commit statuses on the PR's head commit.
async fn checks(
    repo: &ForgeRepo,
    account: &Account,
    pull: &Current,
) -> Result<Vec<(String, Check)>> {
    let head = pull.head_sha.as_deref().context("PR has no head commit")?;
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

/// Each comment or review by its `id_field`, with what changes when it is
/// edited, answered or dismissed and its author.
fn entries(items: &[Value], id_field: &str) -> Result<Vec<(String, Entry)>> {
    items
        .iter()
        .map(|item| {
            let id = item.get(id_field).context("comment or review has no ID")?;
            let entry = Entry::new(comment_fingerprint(id, item), login(&item["user"]));
            Ok((id_text(id), entry))
        })
        .collect()
}

/// The fingerprint of a comment or review with ID `id`; `gh` never set
/// `lastEditedAt`, which snapshots saved from its reports still hold.
fn comment_fingerprint(id: &Value, item: &Value) -> String {
    json!({
        "id": id,
        "body": item["body"],
        "state": item["state"],
        "lastEditedAt": null,
    })
    .to_string()
}

#[cfg(test)]
mod tests;
