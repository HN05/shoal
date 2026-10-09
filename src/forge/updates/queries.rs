use super::{Check, Snapshot};
use crate::forge::{
    ForgeKind, ForgeRepo, Query, commit_revision, fj_merged,
    pr::state::{CheckResult, PrState, PrStatus, Verdict, fj_reviews, github_reviews, summarize},
    strip_bidi_isolates,
};
use anyhow::{Context, Result, anyhow, ensure};
use std::path::Path;

impl ForgeRepo {
    pub(crate) async fn issue_activity(&self, path: &Path, number: u64) -> Result<Snapshot> {
        let id = number.to_string();
        let repository = format!("{}/{}", self.host, self.path);
        let mut snapshot = Snapshot::default();
        match self.kind {
            ForgeKind::GitHub => {
                let args = [
                    "issue",
                    "view",
                    &id,
                    "--repo",
                    &repository,
                    "--json",
                    "number,state,comments",
                ];
                let output = self.kind.query(path, &args, Query::Issue).await?;
                let issue: serde_json::Value = serde_json::from_str(&output)?;
                ensure!(issue["number"] == number, "gh returned a different issue");
                snapshot.state = issue["state"]
                    .as_str()
                    .context("issue has no state")?
                    .to_ascii_lowercase();
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
                        .insert(id.to_string(), comment_fingerprint(comment));
                }
            }
            ForgeKind::Forgejo => {
                let args = [
                    "--style", "minimal", "issue", "view", &id, "--host", &self.host, "--remote",
                    "origin",
                ];
                let output = self.kind.query(path, &args, Query::Issue).await?;
                snapshot.state = super::super::issue::state(self.kind, &output, number)?;
                let args = [&args[..], &["comments"]].concat();
                snapshot.record_comments(
                    "discussion",
                    self.kind.query(path, &args, Query::Issue).await,
                );
            }
        }
        Ok(snapshot)
    }

    pub(crate) async fn activity(
        &self,
        path: &Path,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Snapshot> {
        match self.kind {
            ForgeKind::GitHub => self.github_activity(path, number, branch).await,
            ForgeKind::Forgejo => self.forgejo_activity(path, number, branch).await,
        }
    }

    async fn github_activity(
        &self,
        path: &Path,
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
            "number,headRefName,headRefOid,state,comments,reviews,statusCheckRollup,mergeable",
        ];
        let output = self.kind.query(path, &args, Query::Pull("")).await?;
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
        let output = self.kind.query(path, &args, Query::Pull("")).await?;
        let pages: Vec<Vec<serde_json::Value>> =
            serde_json::from_str(&output).context("invalid gh review comments response")?;
        for comment in pages.into_iter().flatten() {
            let id = comment.get("id").context("review comment has no ID")?;
            snapshot
                .comments
                .insert(format!("inline:{id}"), comment_fingerprint(&comment));
        }
        Ok(snapshot)
    }

    async fn forgejo_activity(
        &self,
        path: &Path,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Snapshot> {
        let id = number.to_string();

        let view = [
            "--style", "minimal", "pr", "view", &id, "--host", &self.host,
        ];
        let output = self.kind.query(path, &view, Query::Pull("")).await?;
        let mut snapshot = Snapshot {
            state: forgejo_state(&output, &id, branch)?.as_str().into(),
            ..Default::default()
        };
        let revision = match self
            .kind
            .query(path, &args_for_commits(&id, &self.host), Query::Pull(""))
            .await
        {
            Ok(output) => match forgejo_revision(&output) {
                Ok(revision) => Some(revision),
                Err(error) => {
                    snapshot
                        .errors
                        .insert("revision".into(), format!("{error:#}"));
                    None
                }
            },
            Err(error) => {
                snapshot
                    .errors
                    .insert("revision".into(), format!("{error:#}"));
                None
            }
        };
        let mut args = view.to_vec();
        args.push("comments");
        snapshot.record_comments(
            "discussion",
            self.kind.query(path, &args, Query::Pull("")).await,
        );
        let args = [
            "--style",
            "minimal",
            "pr",
            "review",
            &id,
            "--host",
            &self.host,
            "list",
            "--comments",
            "--all",
        ];
        snapshot.record_comments(
            "reviews",
            self.kind.query(path, &args, Query::Pull("")).await,
        );
        if snapshot.state == "open" {
            match self.forgejo_checks(path, &id).await {
                Ok(checks) => checks.record(&mut snapshot, revision.as_deref().unwrap_or_default()),
                Err(error) => {
                    snapshot
                        .errors
                        .insert("CI and merge conflicts".into(), format!("{error:#}"));
                }
            }
        }
        Ok(snapshot)
    }

    /// `fj pr status` 0.6 reads CI from the PR commit with the newest timestamp. When a
    /// rebase gives every commit the same timestamp it picks the oldest, and fails when
    /// that commit has no CI. Forgejo's API reads the head commit, so it is the fallback.
    async fn forgejo_checks(&self, path: &Path, id: &str) -> Result<ForgejoChecks> {
        let args = [
            "--style", "minimal", "pr", "status", id, "--host", &self.host,
        ];
        let error = match self.kind.query(path, &args, Query::Pull("")).await {
            Ok(output) => match fj_checks(&output) {
                Ok(checks) => return Ok(checks),
                Err(error) => error,
            },
            Err(error) => error,
        };
        self.forgejo_api_checks(id)
            .await
            .map_err(|fallback| anyhow!("{error:#}; the Forgejo API also failed: {fallback:#}"))
    }

    async fn forgejo_api_checks(&self, id: &str) -> Result<ForgejoChecks> {
        let pull = self.forgejo_api(&format!("pulls/{id}")).await?;
        let head = pull["head"]["sha"]
            .as_str()
            .context("Forgejo PR has no head commit")?;
        let combined = self
            .forgejo_api(&format!("commits/{head}/status?limit=50"))
            .await?;
        api_checks(&pull, &combined)
    }
}

impl ForgeRepo {
    /// Each failed lookup is recorded on the result instead of failing it.
    pub(crate) async fn pr_status(&self, path: &Path, url: &str) -> PrStatus {
        let result = async {
            let (number, _) = self.pull(url)?;
            match self.kind {
                ForgeKind::GitHub => self.github_pr_status(path, number, url).await,
                ForgeKind::Forgejo => self.forgejo_pr_status(path, number, url).await,
            }
        }
        .await;
        result.unwrap_or_else(|error| PrStatus::failed(url.into(), &error))
    }

    async fn github_pr_status(&self, path: &Path, number: u64, url: &str) -> Result<PrStatus> {
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
        let output = self.kind.query(path, &args, Query::Pull("")).await?;
        github_status(&output, number, url)
    }

    async fn forgejo_pr_status(&self, path: &Path, number: u64, url: &str) -> Result<PrStatus> {
        let id = number.to_string();
        let view = [
            "--style", "minimal", "pr", "view", &id, "--host", &self.host,
        ];
        let output = self.kind.query(path, &view, Query::Pull("")).await?;
        let mut status = PrStatus::new(url.into(), Some(forgejo_state(&output, &id, None)?));
        if status.state == Some(PrState::Open) {
            match self.forgejo_checks(path, &id).await {
                Ok(checks) => {
                    let mut snapshot = Snapshot::default();
                    checks.record(&mut snapshot, "");
                    status.merge_conflicts = snapshot.conflict;
                    status.checks = check_results(snapshot);
                }
                Err(error) => status
                    .errors
                    .push(format!("CI and merge conflicts lookup failed: {error:#}")),
            }
        }
        let args = [
            "--style", "minimal", "pr", "review", &id, "--host", &self.host, "list",
        ];
        let reviews = async {
            let output = self.kind.query(path, &args, Query::Pull("")).await?;
            fj_reviews(&output)
        }
        .await;
        record_reviews(&mut status, reviews);
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

fn forgejo_state(text: &str, number: &str, branch: Option<&str>) -> Result<PrState> {
    Ok(if fj_merged(text, number, branch)? {
        PrState::Merged
    } else if strip_bidi_isolates(text)
        .lines()
        .nth(1)
        .is_some_and(|s| s.contains(" — Closed"))
    {
        PrState::Closed
    } else {
        PrState::Open
    })
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
        ..Default::default()
    };
    for field in ["comments", "reviews"] {
        for item in pull[field]
            .as_array()
            .context("missing PR comments or reviews")?
        {
            let id = item.get("id").context("PR comment or review has no ID")?;
            snapshot
                .comments
                .insert(format!("{field}:{id}"), comment_fingerprint(item));
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

fn args_for_commits<'a>(id: &'a str, host: &'a str) -> [&'a str; 8] {
    [
        "--style", "minimal", "pr", "view", id, "--host", host, "commits",
    ]
}

fn forgejo_revision(text: &str) -> Result<String> {
    text.lines()
        .find_map(commit_revision)
        .context("Forgejo PR commits did not include a revision")
}

/// Mergeability and CI contexts of an open PR, in `fj pr status` terms.
struct ForgejoChecks {
    conflict: bool,
    /// Each context with its capitalized state.
    checks: Vec<(String, String)>,
}

impl ForgejoChecks {
    fn record(self, snapshot: &mut Snapshot, revision: &str) {
        snapshot.conflict = Some(self.conflict);
        for (name, state) in self.checks {
            snapshot.checks.insert(
                name.clone(),
                Check {
                    name,
                    complete: state != "Pending",
                    state,
                    revision: revision.into(),
                },
            );
        }
    }
}

fn fj_checks(text: &str) -> Result<ForgejoChecks> {
    let text = strip_bidi_isolates(text).replace("STYLE()", "");
    let mut lines = text.lines();
    let header = lines.next().context("missing fj PR status")?;
    ensure!(
        matches!(
            header,
            "Open — Can be merged" | "Open — Merge conflicts" | "Draft — Can't merge draft PR"
        ),
        "unrecognized fj PR status"
    );
    let mut checks = ForgejoChecks {
        conflict: header == "Open — Merge conflicts",
        checks: Vec::new(),
    };
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let (state, name) = line
            .trim()
            .trim_start_matches("- ")
            .split_once(" — ")
            .context("unrecognized fj CI check")?;
        ensure!(
            matches!(
                state,
                "Pending" | "Success" | "Failure" | "Warning" | "Skipped" | "Error"
            ),
            "unknown fj CI state"
        );
        checks.checks.push((name.into(), state.into()));
    }
    Ok(checks)
}

/// Mirrors `fj pr status`: a `WIP:` title marks a draft, which reports no conflicts.
fn api_checks(pull: &serde_json::Value, combined: &serde_json::Value) -> Result<ForgejoChecks> {
    let draft = pull["title"]
        .as_str()
        .context("Forgejo PR has no title")?
        .starts_with("WIP:");
    let mergeable = pull["mergeable"]
        .as_bool()
        .context("Forgejo PR has no mergeability")?;
    // A commit without statuses has `"statuses": null`.
    let statuses = match &combined["statuses"] {
        serde_json::Value::Null => &[][..],
        statuses => statuses.as_array().context("invalid Forgejo CI statuses")?,
    };
    ensure!(
        combined["total_count"].as_u64() == Some(statuses.len() as u64),
        "Forgejo returned a partial page of CI statuses"
    );
    let mut checks = ForgejoChecks {
        conflict: !draft && !mergeable,
        checks: Vec::new(),
    };
    for status in statuses {
        let name = status["context"]
            .as_str()
            .context("Forgejo CI status has no context")?;
        let state = match status["status"].as_str() {
            Some("pending") => "Pending",
            Some("success") => "Success",
            Some("failure") => "Failure",
            Some("warning") => "Warning",
            Some("skipped") => "Skipped",
            Some("error") => "Error",
            _ => anyhow::bail!("unknown Forgejo CI state"),
        };
        checks.checks.push((name.into(), state.into()));
    }
    Ok(checks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::updates::UpdateKind;
    use serde_json::json;

    fn forgejo_status(text: &str, snapshot: &mut Snapshot, revision: &str) -> Result<()> {
        fj_checks(text).map(|checks| checks.record(snapshot, revision))
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
        let observed = github_snapshot(&pull.to_string(), 7, Some("topic")).unwrap();
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
    fn forgejo_state_distinguishes_open_closed_and_merged() {
        let output = "Title #56\nBy user — Open — +1 -0\nFrom `feature` into `main`\n";
        assert_eq!(forgejo_state(output, "56", None).unwrap(), PrState::Open);
        for (word, state) in [("Closed", PrState::Closed), ("Merged", PrState::Merged)] {
            let output = output.replace("Open", word);
            assert_eq!(forgejo_state(&output, "56", None).unwrap(), state);
        }
        assert!(forgejo_state(output, "57", None).is_err());
    }

    #[test]
    fn forgejo_contexts_are_distinct_and_unknown_output_fails() {
        let mut snapshot = Snapshot::default();
        forgejo_status("\u{2068}Open — Merge conflicts\u{2069}\n- Success — review/default\n- Pending — rust\n- Skipped — deploy\n", &mut snapshot, "rev-1").unwrap();
        let updates = Snapshot::default().changes(&snapshot, "pr");
        assert_eq!(
            updates.iter().map(|u| u.kind).collect::<Vec<_>>(),
            [
                UpdateKind::CiCompleted,
                UpdateKind::CiCompleted,
                UpdateKind::MergeConflict
            ]
        );
        let mut next = snapshot.clone();
        forgejo_status("Open — Merge conflicts\n- Success — review/default\n- Failure — rust\n- Skipped — deploy\n", &mut next, "rev-1").unwrap();
        assert_eq!(snapshot.changes(&next, "pr")[0].message, "rust: Failure");
        assert!(forgejo_status("unknown", &mut next, "rev-1").is_err());
        assert!(
            forgejo_status("Open — Can be merged\n- Unknown — rust", &mut next, "rev-1").is_err()
        );
    }

    #[test]
    fn forgejo_minimal_style_placeholders_preserve_check_names() {
        let mut snapshot = Snapshot::default();
        forgejo_status(
            "\u{2068}STYLE()\u{2069}Open — Can be merged\n- \u{2068}Pending\u{2069} — rust\n- \u{2068}STYLE()\u{2069}Skipped\u{2068}STYLE()\u{2069} — review / STYLE()\n",
            &mut snapshot,
            "rev-1",
        )
        .unwrap();
        assert_eq!(snapshot.conflict, Some(false));
        let updates = Snapshot::default().changes(&snapshot, "pr");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
        assert_eq!(updates[0].message, "review /: Skipped");
        assert!(
            forgejo_status(
                "STYLE()Open — Can be merged\n- STYLE()UnknownSTYLE() — rust",
                &mut snapshot,
                "rev-1",
            )
            .is_err()
        );
    }

    #[test]
    fn forgejo_same_conclusion_on_a_new_revision_wakes_again() {
        let status = "Open — Can be merged\n- Success — rust\n";
        let mut first = Snapshot::default();
        forgejo_status(status, &mut first, "revision-one").unwrap();
        let mut second = Snapshot::default();
        forgejo_status(status, &mut second, "revision-two").unwrap();
        let updates = first.changes(&second, "pr");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
    }

    #[test]
    fn failed_revision_on_first_poll_defers_ci_until_revision_recovers() {
        let previous = Snapshot::default();
        let mut snapshot = Snapshot::default();
        forgejo_status(
            "Open — Merge conflicts\n- Success — rust\n",
            &mut snapshot,
            "",
        )
        .unwrap();
        snapshot
            .errors
            .insert("revision".into(), "unavailable".into());
        snapshot.comments.insert("reviews".into(), "finding".into());
        snapshot.retain_failed_checks(&previous);
        assert!(snapshot.checks.is_empty());
        let updates = previous.changes(&snapshot, "pr");
        assert_eq!(
            updates.iter().map(|update| update.kind).collect::<Vec<_>>(),
            [
                UpdateKind::Comment,
                UpdateKind::MergeConflict,
                UpdateKind::LookupFailed
            ]
        );
        let mut recovered = snapshot.clone();
        recovered.errors.clear();
        forgejo_status(
            "Open — Merge conflicts\n- Success — rust\n",
            &mut recovered,
            "rev-1",
        )
        .unwrap();
        let updates = snapshot.changes(&recovered, "pr");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
    }

    #[test]
    fn failed_status_preserves_checks_and_conflicts_while_comments_wake() {
        let mut before = Snapshot::default();
        forgejo_status(
            "Open — Merge conflicts\n- Success — review",
            &mut before,
            "rev-1",
        )
        .unwrap();
        let mut failed = Snapshot::default();
        failed.errors.insert(
            "CI and merge conflicts".into(),
            "no status available".into(),
        );
        failed
            .comments
            .insert("discussion".into(), "new finding".into());
        failed.retain_failed_checks(&before);
        let updates = before.changes(&failed, "pr");
        assert_eq!(
            updates.iter().map(|u| u.kind).collect::<Vec<_>>(),
            [UpdateKind::Comment, UpdateKind::LookupFailed]
        );
        assert!(failed.changes(&failed, "pr").is_empty());
        let mut recovered = failed.clone();
        recovered.errors.clear();
        forgejo_status(
            "Open — Merge conflicts\n- Success — review",
            &mut recovered,
            "rev-1",
        )
        .unwrap();
        assert!(failed.changes(&recovered, "pr").is_empty());
        let mut unknown = Snapshot::default();
        unknown.retain_failed_checks(&recovered);
        assert_eq!(unknown.conflict, Some(true));
    }

    #[test]
    fn forgejo_api_checks_match_fj_status() {
        let pull = json!({"title": "Topic", "mergeable": false, "head": {"sha": "abc"}});
        let combined = json!({"total_count": 2, "statuses": [
            {"context": "ci / rust", "status": "success"},
            {"context": "review", "status": "pending"}
        ]});
        let mut api = Snapshot::default();
        api_checks(&pull, &combined)
            .unwrap()
            .record(&mut api, "rev-1");
        let mut fj = Snapshot::default();
        forgejo_status(
            "Open — Merge conflicts\n- Success — ci / rust\n- Pending — review\n",
            &mut fj,
            "rev-1",
        )
        .unwrap();
        assert_eq!(api.conflict, Some(true));
        assert!(fj.changes(&api, "pr").is_empty());
        assert_eq!(api.checks, fj.checks);

        let draft = json!({"title": "WIP: Topic", "mergeable": false});
        let none = json!({"total_count": 0, "statuses": null});
        let checks = api_checks(&draft, &none).unwrap();
        assert!(!checks.conflict && checks.checks.is_empty());
        let partial = json!({"total_count": 3, "statuses": combined["statuses"]});
        assert!(api_checks(&pull, &partial).is_err());
        let unknown = json!({"total_count": 1, "statuses": [{"context": "rust", "status": ""}]});
        assert!(api_checks(&pull, &unknown).is_err());
    }
}
