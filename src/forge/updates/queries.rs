use super::{Check, MergeRevision, Snapshot};
use crate::forge::{
    ForgeKind, ForgeRepo, Query,
    account::Account,
    gh,
    item::{Current, ItemState},
    link::ItemKind,
    pr::state::{CheckResult, PrState, PrStatus, Verdict, github_reviews, summarize},
    rest::RestCheck,
};
use anyhow::{Context, Result, ensure};

impl ForgeRepo {
    pub(crate) async fn issue_activity(&self, account: &Account, number: u64) -> Result<Snapshot> {
        match self.kind {
            ForgeKind::GitHub => self.github_issue_activity(account, number).await,
            ForgeKind::Forgejo | ForgeKind::GitLab | ForgeKind::Bitbucket => self
                .rest_issue_activity(account, number)
                .await
                .context(self.kind.login_hint()),
        }
    }

    async fn github_issue_activity(&self, account: &Account, number: u64) -> Result<Snapshot> {
        let id = number.to_string();
        let repository = format!("{}/{}", self.host, self.path);
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
                .insert(super::discussion_key(id), comment_fingerprint(comment));
        }
        Ok(snapshot)
    }

    async fn rest_issue_activity(&self, account: &Account, number: u64) -> Result<Snapshot> {
        let issue = self.read(account, ItemKind::Issue, number).await?;
        let mut snapshot = Snapshot {
            state: issue.item.state.as_str().into(),
            ..Default::default()
        };
        snapshot.record_comments(
            super::DISCUSSION,
            self.rest_discussion(account, ItemKind::Issue, number).await,
        );
        Ok(snapshot)
    }

    pub(crate) async fn activity(
        &self,
        account: &Account,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Snapshot> {
        match self.kind {
            ForgeKind::GitHub => self.github_activity(account, number, branch).await,
            ForgeKind::Forgejo | ForgeKind::GitLab | ForgeKind::Bitbucket => {
                self.rest_activity(account, number, branch).await
            }
        }
    }

    async fn github_activity(
        &self,
        account: &Account,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Snapshot> {
        let id = number.to_string();

        let repository = format!("{}/{}", self.host, self.path);
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
        let mut snapshot = github_snapshot(&output, number, branch)?;
        let endpoint = format!("repos/{}/pulls/{number}/comments", self.path);
        let args = [
            "api",
            "--hostname",
            &self.host,
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
                format!("inline:{}", super::id_text(id)),
                comment_fingerprint(&comment),
            );
        }
        Ok(snapshot)
    }

    async fn rest_activity(
        &self,
        account: &Account,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Snapshot> {
        let pull = self.rest_branch_pull(account, number, branch).await?;
        let mut snapshot = Snapshot {
            state: pull.item.state.as_str().into(),
            ..Default::default()
        };
        snapshot.record_comments(
            super::DISCUSSION,
            self.rest_discussion(account, ItemKind::Pr, number).await,
        );
        snapshot.record_comments("reviews", self.rest_review_activity(account, number).await);
        if pull.item.state == ItemState::Open {
            snapshot.record_checks(&pull, self.rest_checks(account, &pull).await);
        }
        Ok(snapshot)
    }
}

impl ForgeRepo {
    /// Each failed lookup is recorded on the result instead of failing it.
    pub(crate) async fn pr_status(&self, account: &Account, url: &str) -> PrStatus {
        let result = async {
            let (number, _) = self.pull(url)?;
            match self.kind {
                ForgeKind::GitHub => self.github_pr_status(account, number, url).await,
                ForgeKind::Forgejo | ForgeKind::GitLab | ForgeKind::Bitbucket => self
                    .rest_pr_status(account, number, url)
                    .await
                    .context(self.kind.login_hint()),
            }
        }
        .await;
        result.unwrap_or_else(|error| PrStatus::failed(url.into(), &error))
    }

    async fn github_pr_status(
        &self,
        account: &Account,
        number: u64,
        url: &str,
    ) -> Result<PrStatus> {
        let id = number.to_string();
        let repository = format!("{}/{}", self.host, self.path);
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
        github_status(&output, number, url)
    }

    async fn rest_pr_status(&self, account: &Account, number: u64, url: &str) -> Result<PrStatus> {
        let pull = self.read(account, ItemKind::Pr, number).await?;
        let state = match pull.item.state {
            ItemState::Open => PrState::Open,
            ItemState::Merged => PrState::Merged,
            ItemState::Closed => PrState::Closed,
        };
        let mut status = PrStatus::new(url.into(), Some(state));
        if state == PrState::Open {
            let mut snapshot = Snapshot::default();
            snapshot.record_checks(&pull, self.rest_checks(account, &pull).await);
            status.errors.extend(
                snapshot
                    .errors
                    .iter()
                    .map(|(source, error)| format!("{source} lookup failed: {error}")),
            );
            status.merge_conflicts = snapshot.conflict;
            status.checks = check_results(snapshot);
        }
        record_reviews(&mut status, self.rest_verdicts(account, number).await);
        Ok(status)
    }
}

fn github_status(text: &str, number: u64, url: &str) -> Result<PrStatus> {
    let snapshot = github_snapshot(text, number, None)?;
    let pull: serde_json::Value = serde_json::from_str(text)?;
    let state = match snapshot.state.as_str() {
        "open" => PrState::Open,
        "merged" => PrState::Merged,
        _ => PrState::Closed,
    };
    let mut status = PrStatus::new(url.into(), Some(state));
    status.merge_conflicts = snapshot.conflict;
    status.checks = check_results(snapshot);
    record_reviews(&mut status, github_reviews(&pull));
    Ok(status)
}

fn check_results(snapshot: Snapshot) -> Vec<CheckResult> {
    snapshot
        .checks
        .into_values()
        .map(|check| CheckResult {
            name: check.name,
            result: check.state.to_ascii_lowercase(),
        })
        .collect()
}

fn record_reviews(status: &mut PrStatus, reviews: Result<Vec<(String, Verdict)>>) {
    match reviews {
        Ok(reviews) => status.review = Some(summarize(reviews)),
        Err(error) => status
            .errors
            .push(format!("reviews lookup failed: {error:#}")),
    }
}

fn github_snapshot(text: &str, number: u64, branch: Option<&str>) -> Result<Snapshot> {
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
    for (field, source) in [("comments", super::DISCUSSION), ("reviews", "reviews")] {
        for item in pull[field]
            .as_array()
            .context("missing PR comments or reviews")?
        {
            let id = item.get("id").context("PR comment or review has no ID")?;
            snapshot.comments.insert(
                format!("{source}:{}", super::id_text(id)),
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

impl Snapshot {
    /// An open PR's mergeability and the CI checks on its head commit. A
    /// failed check lookup keeps the previous checks.
    fn record_checks(&mut self, pull: &Current, checks: Result<Vec<RestCheck>>) {
        self.conflict = pull.conflict;
        self.conflict_at = MergeRevision {
            head: pull.head_sha.clone().unwrap_or_default(),
            base: pull.base_sha.clone().unwrap_or_default(),
        };
        match checks {
            Ok(checks) => {
                for check in checks {
                    self.checks.insert(
                        check.name.clone(),
                        Check {
                            complete: check.state != "Pending",
                            state: check.state.into(),
                            name: check.name,
                            revision: pull.head_sha.clone().unwrap_or_default(),
                        },
                    );
                }
            }
            Err(error) => {
                self.errors.insert("CI".into(), format!("{error:#}"));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::updates::UpdateKind;
    use serde_json::json;

    /// An open Forgejo PR at `head` with `conflict` and `checks` recorded.
    fn rest_snapshot(head: &str, conflict: bool, checks: &[(&str, &'static str)]) -> Snapshot {
        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        let pull = json!({"number": 7, "title": "Fix", "state": "open", "mergeable": !conflict,
            "head": {"ref": "topic", "sha": head}, "base": {"ref": "main"}});
        let pull = forgejo.current(ItemKind::Pr, 7, &pull).unwrap();
        let checks = checks
            .iter()
            .map(|(name, state)| RestCheck {
                name: (*name).into(),
                state,
            })
            .collect();
        let mut snapshot = Snapshot::default();
        snapshot.record_checks(&pull, Ok(checks));
        snapshot
    }

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
        let before = github_snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        pull["statusCheckRollup"][0]["startedAt"] = json!("rerun");
        let next = github_snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        let updates = before.changes(&next, "pr");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
    }

    #[test]
    fn github_comment_reactions_do_not_wake_but_edits_and_review_state_do() {
        let mut pull = github(json!([]));
        pull["comments"] = json!([{"id": "comment", "body": "hello", "reactionGroups": []}]);
        pull["reviews"] = json!([{"id": "review", "body": "", "state": "COMMENTED"}]);
        let before = github_snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        pull["comments"][0]["reactionGroups"] = json!([{"content": "THUMBS_UP"}]);
        let reacted = github_snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        assert!(before.changes(&reacted, "pr").is_empty());
        pull["comments"][0]["body"] = json!("edited");
        let edited = github_snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        assert_eq!(reacted.changes(&edited, "pr")[0].kind, UpdateKind::Comment);
        pull["reviews"][0]["state"] = json!("APPROVED");
        let approved = github_snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
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
        let before = Snapshot::default();
        let mut observed = github_snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
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
        let next = github_snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        let updates = observed.changes(&next, "pr");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].message, "rust: FAILURE");

        pull["headRefOid"] = json!("def");
        let next_head = github_snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        assert_eq!(
            next.changes(&next_head, "pr")
                .iter()
                .filter(|u| u.kind == UpdateKind::CiCompleted)
                .count(),
            2
        );
        pull["state"] = json!("CLOSED");
        let closed = github_snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
        assert_eq!(next_head.changes(&closed, "pr")[0].kind, UpdateKind::Closed);
        assert!(github_snapshot(&pull.to_string(), 8, Some("topic")).is_err());
        assert!(github_snapshot(&pull.to_string(), 7, Some("another")).is_err());
        assert!(github_snapshot(&pull.to_string(), 7, None).is_ok());
        assert!(github_snapshot(&pull.to_string(), 8, None).is_err());
    }

    #[test]
    fn github_pr_status_reports_state_checks_and_review() {
        let mut pull = github(json!([
            {"__typename": "CheckRun", "name": "rust", "status": "IN_PROGRESS", "conclusion": "", "detailsUrl": "rust/1"},
            {"__typename": "StatusContext", "context": "deploy", "state": "SUCCESS", "targetUrl": "deploy"}
        ]));
        pull["mergeable"] = json!("CONFLICTING");
        pull["reviews"] = json!([{"id": "r", "author": {"login": "a"}, "state": "APPROVED"}]);
        let status = github_status(&pull.to_string(), 7, "url").unwrap();
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
            Some(crate::forge::pr::state::ReviewState::Approved)
        );
        assert!(status.errors.is_empty());

        pull["reviews"][0]["state"] = json!("LATER");
        let status = github_status(&pull.to_string(), 7, "url").unwrap();
        assert_eq!(status.review, None);
        assert_eq!(status.checks.len(), 2);
        assert!(status.errors[0].starts_with("reviews lookup failed"));
        assert!(github_status(&pull.to_string(), 8, "url").is_err());
    }

    #[test]
    fn rest_checks_are_distinct_and_wake_when_they_complete() {
        let mut snapshot = rest_snapshot(
            "rev-1",
            true,
            &[
                ("review/default", "Success"),
                ("rust", "Pending"),
                ("deploy", "Skipped"),
            ],
        );
        snapshot.schedule_conflict_reports(&Snapshot::default());
        let updates = Snapshot::default().changes(&snapshot, "pr");
        assert_eq!(
            updates.iter().map(|u| u.kind).collect::<Vec<_>>(),
            [
                UpdateKind::CiCompleted,
                UpdateKind::CiCompleted,
                UpdateKind::MergeConflict
            ]
        );
        let next = rest_snapshot(
            "rev-1",
            true,
            &[
                ("review/default", "Success"),
                ("rust", "Failure"),
                ("deploy", "Skipped"),
            ],
        );
        let updates = snapshot.changes(&next, "pr");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].message, "rust: Failure");
        assert_eq!(updates[0].check.as_ref().unwrap().result, "failure");
    }

    #[test]
    fn rest_same_conclusion_on_a_new_head_wakes_again() {
        let first = rest_snapshot("revision-one", false, &[("rust", "Success")]);
        let second = rest_snapshot("revision-two", false, &[("rust", "Success")]);
        let updates = first.changes(&second, "pr");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
    }

    #[test]
    fn failed_ci_lookup_preserves_checks_while_comments_and_conflicts_wake() {
        let before = rest_snapshot("rev-1", false, &[("review", "Success")]);
        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        let pull = json!({"number": 7, "title": "Fix", "state": "open", "mergeable": false,
            "head": {"ref": "topic", "sha": "rev-1"}, "base": {"ref": "main"}});
        let pull = forgejo.current(ItemKind::Pr, 7, &pull).unwrap();
        let mut failed = Snapshot::default();
        failed.record_checks(&pull, Err(anyhow::anyhow!("no status available")));
        failed
            .comments
            .insert("discussion:1".into(), "new finding".into());
        failed.retain_failed_checks(&before);
        failed.schedule_conflict_reports(&before);
        let updates = before.changes(&failed, "pr");
        assert_eq!(
            updates.iter().map(|u| u.kind).collect::<Vec<_>>(),
            [
                UpdateKind::Comment,
                UpdateKind::MergeConflict,
                UpdateKind::LookupFailed
            ]
        );
        assert_eq!(updates[2].message, "CI lookup failed: no status available");
        assert!(failed.changes(&failed, "pr").is_empty());
        let mut recovered = rest_snapshot("rev-1", true, &[("review", "Success")]);
        recovered
            .comments
            .insert("discussion:1".into(), "new finding".into());
        assert!(failed.changes(&recovered, "pr").is_empty());
        let mut unknown = Snapshot::default();
        unknown.schedule_conflict_reports(&recovered);
        assert_eq!(unknown.conflict, Some(true));
    }

    #[test]
    fn a_forgejo_conflict_is_reported_again_after_its_base_moves() {
        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        let observe = |previous: &Snapshot, head: &str, base: &str| {
            let pull = json!({"number": 7, "title": "Fix", "state": "open", "mergeable": false,
                "head": {"ref": "topic", "sha": head}, "base": {"ref": "main", "sha": base}});
            let pull = forgejo.current(ItemKind::Pr, 7, &pull).unwrap();
            let mut snapshot = Snapshot::default();
            snapshot.record_checks(&pull, Ok(Vec::new()));
            snapshot.schedule_conflict_reports(previous);
            let kinds = previous
                .changes(&snapshot, "pr")
                .iter()
                .map(|update| update.kind)
                .collect::<Vec<_>>();
            (snapshot, kinds)
        };
        let (before, _) = observe(&Snapshot::default(), "rebased", "main-1");
        let (returned, kinds) = observe(&before, "rebased", "main-2");
        assert!(kinds.is_empty());
        let (_, kinds) = observe(&returned, "rebased", "main-2");
        assert_eq!(kinds, [UpdateKind::MergeConflict]);
    }

    #[test]
    fn forgejo_drafts_report_no_conflicts() {
        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        let pull = json!({"number": 7, "title": "WIP: Fix", "state": "open", "mergeable": false,
            "head": {"ref": "topic", "sha": "rev-1"}, "base": {"ref": "main"}});
        let pull = forgejo.current(ItemKind::Pr, 7, &pull).unwrap();
        assert_eq!(pull.conflict, Some(false));
        let mut snapshot = Snapshot::default();
        snapshot.record_checks(&pull, Ok(Vec::new()));
        assert!(Snapshot::default().changes(&snapshot, "pr").is_empty());
    }
}
