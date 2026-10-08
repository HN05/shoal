use super::{Check, Snapshot};
use crate::forge::{ForgeKind, ForgeRepo, Query, commit_revision, fj_merged, strip_bidi_isolates};
use anyhow::{Context, Result, ensure};
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
        branch: &str,
    ) -> Result<Snapshot> {
        match self.kind {
            ForgeKind::GitHub => self.github_activity(path, number, branch).await,
            ForgeKind::Forgejo => self.forgejo_activity(path, number, branch).await,
        }
    }

    async fn github_activity(&self, path: &Path, number: u64, branch: &str) -> Result<Snapshot> {
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

    async fn forgejo_activity(&self, path: &Path, number: u64, branch: &str) -> Result<Snapshot> {
        let id = number.to_string();

        let view = [
            "--style", "minimal", "pr", "view", &id, "--host", &self.host,
        ];
        let output = self.kind.query(path, &view, Query::Pull("")).await?;
        let merged = fj_merged(&output, &id, branch)?;
        let mut snapshot = Snapshot {
            state: if merged {
                "merged"
            } else if strip_bidi_isolates(&output)
                .lines()
                .nth(1)
                .is_some_and(|s| s.contains(" — Closed"))
            {
                "closed"
            } else {
                "open"
            }
            .into(),
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
            let args = [
                "--style", "minimal", "pr", "status", &id, "--host", &self.host,
            ];
            let result = async {
                let output = self.kind.query(path, &args, Query::Pull("")).await?;
                forgejo_status(
                    &output,
                    &mut snapshot,
                    revision.as_deref().unwrap_or_default(),
                )
            }
            .await;
            if let Err(error) = result {
                snapshot
                    .errors
                    .insert("CI and merge conflicts".into(), format!("{error:#}"));
            }
        }
        Ok(snapshot)
    }
}

fn github_snapshot(text: &str, number: u64, branch: &str) -> Result<Snapshot> {
    let pull: serde_json::Value = serde_json::from_str(text)?;
    ensure!(
        pull["number"] == number && pull["headRefName"] == branch,
        "PR does not match the workspace branch"
    );
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

fn forgejo_status(text: &str, snapshot: &mut Snapshot, revision: &str) -> Result<()> {
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
    snapshot.conflict = Some(header == "Open — Merge conflicts");
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
        snapshot.checks.insert(
            name.into(),
            Check {
                name: name.into(),
                state: state.into(),
                revision: revision.into(),
                complete: state != "Pending",
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::updates::UpdateKind;
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
        let before = github_snapshot(&pull.to_string(), 7, "topic").unwrap();
        pull["statusCheckRollup"][0]["startedAt"] = json!("rerun");
        let next = github_snapshot(&pull.to_string(), 7, "topic").unwrap();
        let updates = before.changes(&next, "pr");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
    }

    #[test]
    fn github_comment_reactions_do_not_wake_but_edits_and_review_state_do() {
        let mut pull = github(json!([]));
        pull["comments"] = json!([{"id": "comment", "body": "hello", "reactionGroups": []}]);
        pull["reviews"] = json!([{"id": "review", "body": "", "state": "COMMENTED"}]);
        let before = github_snapshot(&pull.to_string(), 7, "topic").unwrap();
        pull["comments"][0]["reactionGroups"] = json!([{"content": "THUMBS_UP"}]);
        let reacted = github_snapshot(&pull.to_string(), 7, "topic").unwrap();
        assert!(before.changes(&reacted, "pr").is_empty());
        pull["comments"][0]["body"] = json!("edited");
        let edited = github_snapshot(&pull.to_string(), 7, "topic").unwrap();
        assert_eq!(reacted.changes(&edited, "pr")[0].kind, UpdateKind::Comment);
        pull["reviews"][0]["state"] = json!("APPROVED");
        let approved = github_snapshot(&pull.to_string(), 7, "topic").unwrap();
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
        let observed = github_snapshot(&pull.to_string(), 7, "topic").unwrap();
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
        let next = github_snapshot(&pull.to_string(), 7, "topic").unwrap();
        let updates = observed.changes(&next, "pr");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].message, "rust: FAILURE");

        pull["headRefOid"] = json!("def");
        let next_head = github_snapshot(&pull.to_string(), 7, "topic").unwrap();
        assert_eq!(
            next.changes(&next_head, "pr")
                .iter()
                .filter(|u| u.kind == UpdateKind::CiCompleted)
                .count(),
            2
        );
        pull["state"] = json!("CLOSED");
        let closed = github_snapshot(&pull.to_string(), 7, "topic").unwrap();
        assert_eq!(next_head.changes(&closed, "pr")[0].kind, UpdateKind::Closed);
        assert!(github_snapshot(&pull.to_string(), 8, "topic").is_err());
        assert!(github_snapshot(&pull.to_string(), 7, "another").is_err());
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
}
