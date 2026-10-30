use super::*;
use crate::updates::UpdateKind;
use serde_json::json;

fn pull(mergeable: Value) -> Current {
    let pull = json!({"number": 7, "title": "Fix", "state": "open", "body": "",
        "head": {"ref": "topic", "sha": "abc"}, "base": {"ref": "main", "sha": "base"},
        "mergeable": mergeable});
    ForgeRepo::parse("git@github.com:team/project.git")
        .unwrap()
        .current(ItemKind::Pr, 7, &pull)
        .unwrap()
}

fn runs(runs: Value) -> Value {
    json!({"total_count": runs.as_array().unwrap().len(), "check_runs": runs})
}

fn statuses(statuses: Value) -> Value {
    json!({"total_count": statuses.as_array().unwrap().len(), "statuses": statuses})
}

/// A snapshot of an open PR with head `abc` and these checks.
fn snapshot(mergeable: Value, runs: &Value, statuses: &Value) -> Snapshot {
    let mut checks = check_runs("abc", runs).unwrap();
    checks.extend(commit_statuses("abc", statuses).unwrap());
    let mut snapshot = Snapshot::new("open");
    snapshot.record_keyed_checks(&pull(mergeable), Ok(checks));
    snapshot
}

#[test]
fn checks_keep_the_keys_states_and_revisions_gh_reported() {
    let runs = runs(json!([
        {"name": "rust", "status": "completed", "conclusion": "failure",
         "details_url": "https://github.com/team/project/actions/runs/1/job/2",
         "started_at": "2026-10-01T10:00:00Z", "completed_at": "2026-10-01T10:05:00Z"},
        {"name": "review", "status": "in_progress", "conclusion": null,
         "details_url": "review/1", "started_at": "2026-10-01T10:00:00Z", "completed_at": null},
    ]));
    let statuses = statuses(json!([
        {"context": "deploy", "state": "success", "target_url": null,
         "created_at": "2026-10-01T11:00:00Z"},
    ]));
    let checks = snapshot(json!(true), &runs, &statuses).checks;
    let check = |name: &str, state: &str, revision: &str, complete| Check {
        name: name.into(),
        state: state.into(),
        revision: revision.into(),
        complete,
    };
    // As `gh pr view --json statusCheckRollup` reported them.
    let expected = [
        (
            ":deploy",
            check(
                "deploy",
                "SUCCESS",
                r#""abc":null:"2026-10-01T11:00:00Z""#,
                true,
            ),
        ),
        (
            "https://github.com/team/project/actions/runs/1/job/2:rust",
            check(
                "rust",
                "FAILURE",
                r#""abc":"2026-10-01T10:05:00Z":"2026-10-01T10:00:00Z""#,
                true,
            ),
        ),
        (
            "review/1:review",
            check(
                "review",
                "IN_PROGRESS",
                r#""abc":null:"2026-10-01T10:00:00Z""#,
                false,
            ),
        ),
    ];
    assert_eq!(
        checks.into_iter().collect::<Vec<_>>(),
        expected.map(|(key, check)| (key.to_owned(), check))
    );
}

#[test]
fn github_status_reruns_on_the_same_head_wake_again() {
    let mut status = json!([{"context": "deploy", "state": "success",
        "target_url": "deploy", "created_at": "first"}]);
    let before = snapshot(json!(true), &runs(json!([])), &statuses(status.clone()));
    status[0]["created_at"] = json!("rerun");
    let next = snapshot(json!(true), &runs(json!([])), &statuses(status));
    let updates = before.changes(&next, "pr");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
}

#[test]
fn completed_checks_wake_individually_while_other_checks_run() {
    let mut checks = json!([
        {"name": "rust", "status": "in_progress", "conclusion": null, "details_url": "rust/1"},
        {"name": "review", "status": "completed", "conclusion": "success", "details_url": "review/1"},
    ]);
    let pending = statuses(json!([{"context": "deploy", "state": "pending"}]));
    let mut observed = snapshot(json!(false), &runs(checks.clone()), &pending);
    let mut first = Snapshot::new("open");
    first.conflict = observed.conflict;
    first.conflict_at = observed.conflict_at.clone();
    observed.normalize(&first, 1000);
    let updates = first.changes(&observed, "pr");
    assert_eq!(
        updates.iter().map(|u| u.kind).collect::<Vec<_>>(),
        [UpdateKind::CiCompleted, UpdateKind::MergeConflict]
    );
    assert_eq!(updates[0].message, "review: SUCCESS");
    assert!(observed.changes(&observed, "pr").is_empty());

    checks[0]["status"] = json!("completed");
    checks[0]["conclusion"] = json!("failure");
    let next = snapshot(json!(false), &runs(checks), &pending);
    let updates = observed.changes(&next, "pr");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].message, "rust: FAILURE");
    assert!(check_runs("abc", &json!({"total_count": 2, "check_runs": []})).is_err());
}

#[test]
fn drafts_report_their_conflicts() {
    let mut value = json!({"number": 7, "title": "Fix", "state": "open", "draft": true,
        "head": {"ref": "topic", "sha": "abc"}, "base": {"ref": "main"}, "mergeable": false});
    let repo = ForgeRepo::parse("git@github.com:team/project.git").unwrap();
    assert_eq!(
        repo.current(ItemKind::Pr, 7, &value).unwrap().conflict,
        Some(true)
    );
    value["mergeable"] = Value::Null;
    assert_eq!(
        repo.current(ItemKind::Pr, 7, &value).unwrap().conflict,
        None
    );
}

#[test]
fn comments_keep_the_fingerprints_gh_reported_and_ignore_reactions() {
    let mut comment = json!({"id": 1, "node_id": "IC_1", "body": "hello",
        "user": {"login": "a"}, "reactions": {}, "updated_at": "first"});
    let (key, entry) = entries(std::slice::from_ref(&comment), "node_id")
        .unwrap()
        .remove(0);
    assert_eq!(key, "IC_1");
    assert_eq!(entry.author, "a");
    // As `gh pr view --json comments` reported it.
    let saved = json!({"id": "IC_1", "body": "hello", "state": null, "lastEditedAt": null});
    assert_eq!(entry.fingerprint, saved.to_string());
    comment["reactions"] = json!({"+1": 1});
    comment["updated_at"] = json!("reacted");
    let reacted = entries(std::slice::from_ref(&comment), "node_id").unwrap();
    assert_eq!(reacted[0].1, entry);
    comment["body"] = json!("edited");
    let edited = entries(std::slice::from_ref(&comment), "node_id").unwrap();
    assert_ne!(edited[0].1, entry);

    let review = json!({"id": 2, "node_id": "PRR_2", "body": "", "state": "COMMENTED"});
    let approved = json!({"id": 2, "node_id": "PRR_2", "body": "", "state": "APPROVED"});
    assert_ne!(
        entries(&[review], "node_id").unwrap(),
        entries(&[approved], "node_id").unwrap()
    );
    let inline = json!({"id": 3, "body": "finding", "user": {"login": "bot"}});
    let saved = json!({"id": 3, "body": "finding", "state": null, "lastEditedAt": null});
    let (key, entry) = entries(&[inline], "id").unwrap().remove(0);
    assert_eq!((key.as_str(), entry.fingerprint), ("3", saved.to_string()));
}
