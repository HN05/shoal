//! GitHub activity and PR status, read with `gh`, which reports comments,
//! reviews, checks and mergeability of a PR in one view.
use anyhow::{Context, Result, ensure};

use super::{Query, gh};
use crate::{
    ForgeRepo,
    account::Account,
    review::status::{PrState, PrStatus, github_reviews},
    updates::{Check, DISCUSSION, MergeRevision, Snapshot, discussion_key, id_text},
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
            .insert(discussion_key(id), comment_fingerprint(comment));
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
            comment_fingerprint(&comment),
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
                comment_fingerprint(item),
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
mod tests {
    use super::*;
    use crate::updates::UpdateKind;
    use serde_json::json;

    fn github(checks: serde_json::Value) -> serde_json::Value {
        json!({"number": 7, "headRefName": "topic", "headRefOid": "abc", "state": "OPEN",
            "comments": [], "reviews": [], "statusCheckRollup": checks, "mergeable": "MERGEABLE"})
    }

    #[test]
    fn github_status_reruns_on_the_same_head_wake_again() {
        let mut pull = github(json!([
            {"__typename": "StatusContext", "context": "deploy", "state": "SUCCESS",
             "targetUrl": "deploy", "startedAt": "first"}
        ]));
        let before = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        pull["statusCheckRollup"][0]["startedAt"] = json!("rerun");
        let next = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        let updates = before.changes(&next, "pr");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
    }

    #[test]
    fn github_comment_reactions_do_not_wake_but_edits_and_review_state_do() {
        let mut pull = github(json!([]));
        pull["comments"] = json!([{"id": "comment", "body": "hello", "reactionGroups": []}]);
        pull["reviews"] = json!([{"id": "review", "body": "", "state": "COMMENTED"}]);
        let before = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        pull["comments"][0]["reactionGroups"] = json!([{"content": "THUMBS_UP"}]);
        let reacted = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        assert!(before.changes(&reacted, "pr").is_empty());
        pull["comments"][0]["body"] = json!("edited");
        let edited = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        assert_eq!(reacted.changes(&edited, "pr")[0].kind, UpdateKind::Comment);
        pull["reviews"][0]["state"] = json!("APPROVED");
        let approved = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        assert_eq!(edited.changes(&approved, "pr")[0].kind, UpdateKind::Comment);
        let mut inline =
            json!({"id": 1, "body": "finding", "reactions": {}, "updated_at": "first"});
        let original = comment_fingerprint(&inline);
        inline["reactions"] = json!({"+1": 1});
        inline["updated_at"] = json!("reacted");
        assert_eq!(original, comment_fingerprint(&inline));
        inline["body"] = json!("edited finding");
        assert_ne!(original, comment_fingerprint(&inline));
    }

    #[test]
    fn completed_checks_wake_individually_while_other_checks_run() {
        let mut pull = github(json!([
            {"__typename": "CheckRun", "name": "rust", "status": "IN_PROGRESS", "conclusion": "", "detailsUrl": "rust/1"},
            {"__typename": "CheckRun", "name": "review", "status": "COMPLETED", "conclusion": "SUCCESS", "detailsUrl": "review/1"},
            {"__typename": "StatusContext", "context": "deploy", "state": "EXPECTED"}
        ]));
        pull["comments"] = json!([{"id": "IC_1", "body": "hello"}]);
        pull["reviews"] = json!([{"id": "PRR_1", "body": "finding"}]);
        pull["mergeable"] = json!("CONFLICTING");
        let mut observed = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        let before = Snapshot {
            conflict: observed.conflict,
            conflict_at: observed.conflict_at.clone(),
            ..Default::default()
        };
        observed.schedule_conflict_reports(&before);
        let updates = before.changes(&observed, "pr");
        assert_eq!(
            updates.iter().map(|u| u.kind).collect::<Vec<_>>(),
            [
                UpdateKind::Comment,
                UpdateKind::CiCompleted,
                UpdateKind::MergeConflict
            ]
        );
        assert_eq!(updates[1].message, "review: SUCCESS");
        assert!(observed.changes(&observed, "pr").is_empty());

        pull["statusCheckRollup"][0]["status"] = json!("COMPLETED");
        pull["statusCheckRollup"][0]["conclusion"] = json!("FAILURE");
        let next = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        let updates = observed.changes(&next, "pr");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].message, "rust: FAILURE");

        pull["headRefOid"] = json!("def");
        let next_head = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        assert_eq!(
            next.changes(&next_head, "pr")
                .iter()
                .filter(|u| u.kind == UpdateKind::CiCompleted)
                .count(),
            2
        );
        pull["state"] = json!("CLOSED");
        let closed = snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        assert_eq!(next_head.changes(&closed, "pr")[0].kind, UpdateKind::Closed);
        assert!(snapshot(&pull.to_string(), 8, Some("topic")).is_err());
        assert!(snapshot(&pull.to_string(), 7, Some("another")).is_err());
        assert!(snapshot(&pull.to_string(), 7, None).is_ok());
        assert!(snapshot(&pull.to_string(), 8, None).is_err());
    }

    #[test]
    fn github_pr_status_reports_state_checks_and_review() {
        let mut pull = github(json!([
            {"__typename": "CheckRun", "name": "rust", "status": "IN_PROGRESS", "conclusion": "", "detailsUrl": "rust/1"},
            {"__typename": "StatusContext", "context": "deploy", "state": "SUCCESS", "targetUrl": "deploy"}
        ]));
        pull["mergeable"] = json!("CONFLICTING");
        pull["reviews"] = json!([{"id": "r", "author": {"login": "a"}, "state": "APPROVED"}]);
        let status = reported_status(&pull.to_string(), 7, "url").unwrap();
        assert_eq!(status.state, Some(PrState::Open));
        assert_eq!(status.merge_conflicts, Some(true));
        assert_eq!(
            status
                .checks
                .iter()
                .map(|check| (check.name.as_str(), check.result.as_str()))
                .collect::<Vec<_>>(),
            [("deploy", "success"), ("rust", "in_progress")]
        );
        assert_eq!(
            status.review,
            Some(crate::review::status::ReviewState::Approved)
        );
        assert!(status.errors.is_empty());

        pull["reviews"][0]["state"] = json!("LATER");
        let status = reported_status(&pull.to_string(), 7, "url").unwrap();
        assert_eq!(status.review, None);
        assert_eq!(status.checks.len(), 2);
        assert!(status.errors[0].starts_with("reviews lookup failed"));
        assert!(reported_status(&pull.to_string(), 8, "url").is_err());
    }
}
