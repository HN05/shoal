//! GitHub activity and PR status, read with `gh`, which reports comments,
//! reviews, checks and mergeability of a PR in one view.
use anyhow::{Context, Result, ensure};

use super::{Query, gh};
use crate::{
    ForgeRepo,
    account::Account,
    review::{
        status::{PrState, PrStatus, github_reviews},
        view::login,
    },
    updates::{Check, DISCUSSION, Entry, MergeRevision, Snapshot, discussion_key, id_text},
};

pub(crate) async fn issue_activity(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<Snapshot> {
    let id = number.to_string();
    let repository = format!("{}/{}", repo.host, repo.path);
    let args = [
        "issue",
        "view",
        &id,
        "--repo",
        &repository,
        "--json",
        "number,state,comments",
    ];
    let output = gh(account, &args, Query::Issue).await?;
    let issue: serde_json::Value = serde_json::from_str(&output)?;
    ensure!(issue["number"] == number, "gh returned a different issue");
    let mut snapshot = Snapshot {
        state: issue["state"]
            .as_str()
            .context("issue has no state")?
            .to_ascii_lowercase(),
        ..Default::default()
    };
    ensure!(
        matches!(snapshot.state.as_str(), "open" | "closed"),
        "unknown issue state"
    );
    for comment in issue["comments"]
        .as_array()
        .context("missing issue comments")?
    {
        let id = comment.get("id").context("issue comment has no ID")?;
        snapshot
            .comments
            .insert(discussion_key(id), entry(comment, &comment["author"]));
    }
    Ok(snapshot)
}

pub(crate) async fn pr_activity(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    branch: Option<&str>,
) -> Result<Snapshot> {
    let id = number.to_string();

    let repository = format!("{}/{}", repo.host, repo.path);
    let args = [
        "pr",
        "view",
        &id,
        "--repo",
        &repository,
        "--json",
        "number,headRefName,headRefOid,baseRefOid,state,comments,reviews,statusCheckRollup,mergeable",
    ];
    let output = gh(account, &args, Query::Pull("")).await?;
    let mut snapshot = snapshot(&output, number, branch)?;
    let endpoint = format!("repos/{}/pulls/{number}/comments", repo.path);
    let args = [
        "api",
        "--hostname",
        &repo.host,
        &endpoint,
        "--paginate",
        "--slurp",
    ];
    let output = gh(account, &args, Query::Pull("")).await?;
    let pages: Vec<Vec<serde_json::Value>> =
        serde_json::from_str(&output).context("invalid gh review comments response")?;
    for comment in pages.into_iter().flatten() {
        let id = comment.get("id").context("review comment has no ID")?;
        snapshot.comments.insert(
            format!("inline:{}", id_text(id)),
            entry(&comment, &comment["user"]),
        );
    }
    Ok(snapshot)
}

pub(crate) async fn pr_status(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    url: &str,
) -> Result<PrStatus> {
    let id = number.to_string();
    let repository = format!("{}/{}", repo.host, repo.path);
    let args = [
        "pr",
        "view",
        &id,
        "--repo",
        &repository,
        "--json",
        "number,headRefOid,state,comments,reviews,statusCheckRollup,mergeable",
    ];
    let output = gh(account, &args, Query::Pull("")).await?;
    reported_status(&output, number, url)
}

fn reported_status(text: &str, number: u64, url: &str) -> Result<PrStatus> {
    let snapshot = snapshot(text, number, None)?;
    let pull: serde_json::Value = serde_json::from_str(text)?;
    let state = match snapshot.state.as_str() {
        "open" => PrState::Open,
        "merged" => PrState::Merged,
        _ => PrState::Closed,
    };
    let mut status = PrStatus::new(url.into(), Some(state));
    status.merge_conflicts = snapshot.conflict;
    status.checks = snapshot.check_results();
    status.record_reviews(github_reviews(&pull));
    Ok(status)
}

fn snapshot(text: &str, number: u64, branch: Option<&str>) -> Result<Snapshot> {
    let pull: serde_json::Value = serde_json::from_str(text)?;
    ensure!(pull["number"] == number, "gh returned a different PR");
    if let Some(branch) = branch {
        ensure!(
            pull["headRefName"] == branch,
            "PR does not match the workspace branch"
        );
    }
    let state = pull["state"]
        .as_str()
        .context("missing PR state")?
        .to_ascii_lowercase();
    ensure!(
        matches!(state.as_str(), "open" | "closed" | "merged"),
        "unknown PR state"
    );
    let mut snapshot = Snapshot {
        state,
        head: pull["headRefOid"].as_str().unwrap_or_default().into(),
        conflict: match pull["mergeable"].as_str() {
            Some("CONFLICTING") => Some(true),
            Some("MERGEABLE") => Some(false),
            Some("UNKNOWN") | None => None,
            _ => anyhow::bail!("unknown PR mergeability"),
        },
        conflict_at: MergeRevision {
            head: pull["headRefOid"].as_str().unwrap_or_default().into(),
            base: pull["baseRefOid"].as_str().unwrap_or_default().into(),
        },
        ..Default::default()
    };
    for (field, source) in [("comments", DISCUSSION), ("reviews", "reviews")] {
        for item in pull[field]
            .as_array()
            .context("missing PR comments or reviews")?
        {
            let id = item.get("id").context("PR comment or review has no ID")?;
            snapshot.comments.insert(
                format!("{source}:{}", id_text(id)),
                entry(item, &item["author"]),
            );
        }
    }
    for item in pull["statusCheckRollup"]
        .as_array()
        .context("missing PR checks")?
    {
        let name = item["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| item["context"].as_str().filter(|s| !s.is_empty()))
            .context("CI check has no name")?;
        let state = item["conclusion"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| item["state"].as_str().filter(|s| !s.is_empty()))
            .or_else(|| item["status"].as_str().filter(|s| !s.is_empty()))
            .context("CI check has no state")?;
        let complete = if item["__typename"] == "CheckRun" {
            item["status"] == "COMPLETED"
        } else {
            matches!(state, "SUCCESS" | "FAILURE" | "ERROR")
        };
        let key = format!(
            "{}:{name}",
            item["detailsUrl"]
                .as_str()
                .or_else(|| item["targetUrl"].as_str())
                .unwrap_or_default()
        );
        snapshot.checks.insert(
            key,
            Check {
                name: name.into(),
                state: state.into(),
                revision: format!(
                    "{}:{}:{}",
                    pull["headRefOid"], item["completedAt"], item["startedAt"]
                ),
                complete,
            },
        );
    }
    Ok(snapshot)
}

/// A comment or review as a snapshot records it, written by `user`.
fn entry(item: &serde_json::Value, user: &serde_json::Value) -> Entry {
    Entry::new(comment_fingerprint(item), login(user))
}

fn comment_fingerprint(item: &serde_json::Value) -> String {
    serde_json::json!({
        "id": item["id"],
        "body": item["body"],
        "state": item["state"],
        "lastEditedAt": item["lastEditedAt"],
    })
    .to_string()
}

#[cfg(test)]
mod tests;
