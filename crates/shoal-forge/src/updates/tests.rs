use super::*;
use crate::items::link::ItemKind;
use serde_json::json;

#[test]
fn activity_changes_report_finished_checks_without_waiting_for_the_set() {
    let mut pending = Snapshot::default();
    let check = |name: &str, complete: bool| Check {
        name: name.into(),
        state: if complete { "success" } else { "pending" }.into(),
        revision: "1".into(),
        complete,
    };
    pending.checks.insert("rust".into(), check("rust", false));
    pending
        .checks
        .insert("review".into(), check("review", false));
    pending.conflict = Some(true);
    assert!(Snapshot::default().changes(&pending, "pr").is_empty());
    let mut reviewed = pending.clone();
    reviewed
        .checks
        .insert("review".into(), check("review", true));
    reviewed.comments.insert("review".into(), "finding".into());
    reviewed.normalize(&pending, 1000);
    let updates = pending.changes(&reviewed, "pr");
    assert_eq!(
        updates.iter().map(|u| u.kind).collect::<Vec<_>>(),
        [
            UpdateKind::Comment,
            UpdateKind::CiCompleted,
            UpdateKind::MergeConflict
        ]
    );
    assert_eq!(updates[1].message, "review: success");
    assert!(reviewed.changes(&reviewed, "pr").is_empty());
    let mut closed = reviewed.clone();
    closed.state = "closed".into();
    assert_eq!(reviewed.changes(&closed, "pr")[0].kind, UpdateKind::Closed);
}

#[test]
fn conflicts_are_reported_again_for_a_new_head_or_base_once_confirmed() {
    let poll = |previous: &Snapshot, head: &str, base: &str, conflict| {
        let mut next = Snapshot {
            conflict,
            conflict_at: MergeRevision {
                head: head.into(),
                base: base.into(),
            },
            ..Default::default()
        };
        next.normalize(previous, 1000);
        let messages = previous
            .changes(&next, "pr")
            .into_iter()
            .map(|update| update.message)
            .collect::<Vec<_>>();
        (next, messages)
    };
    let (seen, messages) = poll(&Snapshot::default(), "a", "m1", Some(true));
    assert!(messages.is_empty());
    let (first, messages) = poll(&seen, "a", "m1", Some(true));
    assert_eq!(messages.len(), 1);
    // Right after a push the forge may still show the old head's result.
    let (pushed, messages) = poll(&first, "b", "m1", Some(true));
    assert!(messages.is_empty());
    let (confirmed, messages) = poll(&pushed, "b", "m1", Some(true));
    assert!(messages[0].contains("after the push"), "{messages:?}");
    let (persisting, messages) = poll(&confirmed, "b", "m1", Some(true));
    assert!(messages.is_empty());
    let (moved, messages) = poll(&persisting, "b", "m2", Some(true));
    assert!(messages.is_empty());
    let (unknown, messages) = poll(&moved, "", "", None);
    assert!(messages.is_empty());
    let (_, messages) = poll(&unknown, "b", "m2", Some(true));
    assert!(messages[0].contains("base branch"), "{messages:?}");
    // A conflict on a mergeable PR also waits for a second poll.
    let (mergeable, messages) = poll(&moved, "c", "m2", Some(false));
    assert!(messages.is_empty());
    let (conflicting, messages) = poll(&mergeable, "c", "m3", Some(true));
    assert!(messages.is_empty());
    let (_, messages) = poll(&conflicting, "c", "m3", Some(true));
    assert_eq!(messages.len(), 1);
}

#[test]
fn a_sources_failures_are_reported_once_per_interval() {
    let observed = |error: Option<&str>| {
        let mut snapshot = Snapshot::default();
        if let Some(error) = error {
            snapshot.errors.insert("CI".into(), error.into());
        }
        snapshot
    };
    let poll = |previous: &Snapshot, error, now| {
        let mut next = observed(error);
        next.normalize(previous, now);
        let kinds = previous
            .changes(&next, "pr")
            .iter()
            .map(|update| update.kind)
            .collect::<Vec<_>>();
        (next, kinds)
    };
    let (first, kinds) = poll(&Snapshot::default(), Some("refused after 2 ms"), 1000);
    assert_eq!(kinds, [UpdateKind::LookupFailed]);
    // Neither another error nor a passing poll in between reports it again.
    let (changed, kinds) = poll(&first, Some("refused after 6 ms"), 1001);
    assert!(kinds.is_empty());
    let (passed, kinds) = poll(&changed, None, 1002);
    assert!(kinds.is_empty());
    let (quiet, kinds) = poll(
        &passed,
        Some("broken pipe"),
        1000 + FAILURE_REPORT_INTERVAL - 1,
    );
    assert!(kinds.is_empty());
    let (again, kinds) = poll(&quiet, Some("broken pipe"), 1000 + FAILURE_REPORT_INTERVAL);
    assert_eq!(kinds, [UpdateKind::LookupFailed]);
    let (recovered, kinds) = poll(&again, None, 1000 + 2 * FAILURE_REPORT_INTERVAL);
    assert!(kinds.is_empty());
    assert!(recovered.reported.is_empty());
}

#[test]
fn an_unread_item_keeps_its_activity_and_reports_its_failure_once() {
    let mut seen = Snapshot::new("open");
    seen.comments.insert("discussion:1".into(), "hello".into());
    let poll = |previous: &Snapshot, now| {
        let refused = anyhow::anyhow!("connection refused");
        let mut next = Snapshot::unread_item("issue", &refused);
        next.normalize(previous, now);
        let updates = previous.changes(&next, "issue");
        (next, updates)
    };
    let (unread, updates) = poll(&seen, 1000);
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert_eq!(updates[0].kind, UpdateKind::LookupFailed);
    assert_eq!(
        updates[0].message,
        "issue lookup failed: connection refused"
    );
    assert_eq!(unread.state, "open");
    assert_eq!(unread.comments, seen.comments);
    let (still, updates) = poll(&unread, 1001);
    assert!(updates.is_empty(), "{updates:?}");
    // Reading the item again reports nothing it already showed.
    let mut read = seen.clone();
    read.normalize(&still, 1002);
    assert!(still.changes(&read, "issue").is_empty());
}

#[test]
fn deleting_one_of_several_comments_is_reported() {
    let mut both = Snapshot::default();
    both.record_comments(
        DISCUSSION,
        Ok(vec![("1".into(), "hi".into()), ("2".into(), "typo".into())]),
    );
    let mut one = Snapshot::default();
    one.record_comments(DISCUSSION, Ok(vec![("1".into(), "hi".into())]));
    assert_eq!(both.changes(&one, "pr")[0].kind, UpdateKind::Comment);
    assert!(one.changes(&one, "pr").is_empty());
}

#[test]
fn comment_updates_count_changes_by_kind_and_author() {
    let entry = |text: &str, author: &str| Entry::new(text.into(), author.into());
    let mut previous = Snapshot::default();
    previous.record_comments(
        DISCUSSION,
        Ok(vec![
            ("1".into(), entry("typo", "alice")),
            ("2".into(), entry("old", "bob")),
        ]),
    );
    previous.record_comments("reviews", Ok(vec![("3".into(), entry("finding", "bot"))]));
    let mut next = Snapshot::default();
    next.record_comments(
        DISCUSSION,
        Ok(vec![
            ("1".into(), entry("fixed", "alice")),
            ("4".into(), entry("rebase", "alice")),
            ("5".into(), entry("and docs", "alice")),
        ]),
    );
    next.record_comments(
        "reviews",
        Ok(vec![
            ("3".into(), entry("finding", "bot")),
            ("6".into(), entry("blocker", "bot")),
        ]),
    );
    next.comments.insert("inline:7".into(), entry("nit", "bot"));
    let updates = previous.changes(&next, "pr");
    assert_eq!(updates.len(), 1);
    assert!(
        updates[0].message.starts_with(
            "2 new comments from alice, 1 new inline comment from bot, 1 new review from bot, \
             1 edited comment from alice, 1 deleted comment from bob;"
        ),
        "{}",
        updates[0].message
    );
    let mut answered = next.clone();
    answered
        .comments
        .insert("reviews:6".into(), entry("blocker, 1 reply", "bot"));
    let message = &next.changes(&answered, "pr")[0].message;
    assert!(
        message.starts_with("1 updated review from bot;"),
        "{message}"
    );
}

#[test]
fn comments_saved_without_authors_match_the_same_comments_with_them() {
    let saved: Snapshot = serde_json::from_value(json!({
        "comments": {"discussion:1": "hi", "discussion:2": "bye"},
        "checks": {}, "conflict": null, "state": "open", "errors": {},
    }))
    .unwrap();
    let mut next = Snapshot::new("open");
    next.record_comments(
        DISCUSSION,
        Ok(vec![("1".into(), Entry::new("hi".into(), "alice".into()))]),
    );
    let message = &saved.changes(&next, "pr")[0].message;
    assert!(message.starts_with("1 deleted comment;"), "{message}");
    let stored: Snapshot = serde_json::from_str(&serde_json::to_string(&next).unwrap()).unwrap();
    assert_eq!(stored.comments, next.comments);
}

#[test]
fn saved_combined_verdicts_match_the_same_verdicts_by_reviewer() {
    let verdicts = |pairs: &[(&str, &str)]| {
        let mut snapshot = Snapshot::new("open");
        let pairs = pairs
            .iter()
            .map(|(reviewer, verdict)| ((*reviewer).to_owned(), (*verdict).to_owned()));
        snapshot.record_comments("reviews", Ok(verdict_entries(pairs)));
        snapshot
    };
    let saved: Snapshot = serde_json::from_value(json!({
        "comments": {"reviews:verdicts": "bob:Approved\nalice:ChangesRequested\nbob:Approved\n"},
        "checks": {}, "conflict": null, "state": "open", "errors": {},
    }))
    .unwrap();
    let both = verdicts(&[
        ("alice", "ChangesRequested"),
        ("bob", "Approved"),
        ("bob", "Approved"),
    ]);
    assert!(saved.changes(&both, "pr").is_empty());
    // Reviewers who share a name still change their entry one at a time.
    let withdrawn = verdicts(&[("alice", "ChangesRequested"), ("bob", "Approved")]);
    let message = &both.changes(&withdrawn, "pr")[0].message;
    assert!(
        message.starts_with("1 updated review from bob;"),
        "{message}"
    );
}

#[test]
fn a_rate_limited_lookup_leaves_the_snapshot_incomplete_without_failing() {
    let limit = RateLimited {
        host: "forge.example".into(),
        until: 1,
    };
    let mut snapshot = Snapshot::default();
    let error = anyhow::Error::new(limit.clone()).context("GET comments");
    snapshot.record_comments(DISCUSSION, Err(error));
    snapshot.record_comments("reviews", Err(anyhow::anyhow!("HTTP 500")));
    assert_eq!(snapshot.errors.keys().collect::<Vec<_>>(), ["reviews"]);
    let error = snapshot.complete().unwrap_err();
    assert_eq!(rate_limited(&error), Some(&limit));
    assert!(Snapshot::default().complete().is_ok());
}

#[test]
fn a_failed_comment_lookup_keeps_that_sources_comments() {
    let mut previous = Snapshot::default();
    previous.record_comments("discussion", Ok(vec![("1".into(), "hi".into())]));
    previous.record_comments("reviews", Ok(vec![("2".into(), "lgtm".into())]));
    previous.record_comments("inline", Ok(vec![("3".into(), "nit".into())]));
    let mut failed = Snapshot::default();
    failed.record_comments("discussion", Err(anyhow::anyhow!("timed out")));
    failed.record_comments("reviews", Ok(Vec::new()));
    failed.record_comments("inline", Err(anyhow::anyhow!("HTTP 500")));
    failed.normalize(&previous, 1000);
    assert_eq!(
        failed.comments.keys().collect::<Vec<_>>(),
        ["discussion:1", "inline:3"]
    );
    let mut recovered = failed.clone();
    recovered.errors.clear();
    assert!(failed.changes(&recovered, "pr").is_empty());
}

#[test]
fn authored_comments_wake_only_once_they_change() {
    let mut previous = Snapshot::default();
    previous.record_comments(DISCUSSION, Ok(vec![("1".into(), "hi".into())]));
    let mut authored = BTreeSet::from(["discussion:2".to_owned(), "discussion:3".to_owned()]);
    let mut next = previous.clone();
    next.record_comments(DISCUSSION, Ok(vec![("2".into(), "mine".into())]));
    previous.adopt_authored(&next, &mut authored);
    assert!(previous.changes(&next, "pr").is_empty());
    assert_eq!(authored, BTreeSet::from(["discussion:3".to_owned()]));
    let mut edited = next.clone();
    edited.record_comments(DISCUSSION, Ok(vec![("2".into(), "edited".into())]));
    next.adopt_authored(&edited, &mut authored);
    assert_eq!(next.changes(&edited, "pr")[0].kind, UpdateKind::Comment);
    let mut others = next.clone();
    others.record_comments(DISCUSSION, Ok(vec![("4".into(), "theirs".into())]));
    next.adopt_authored(&others, &mut authored);
    assert_eq!(next.changes(&others, "pr")[0].kind, UpdateKind::Comment);
}

#[test]
fn an_authored_reply_to_an_existing_review_is_taken_once_it_changes() {
    let mut previous = Snapshot::default();
    previous.record_comments("reviews", Ok(vec![("7".into(), "1 comment".into())]));
    let mut authored = BTreeSet::from(["reviews:7".to_owned()]);
    // A poll that read the review before the reply keeps waiting for it.
    previous.adopt_authored(&previous.clone(), &mut authored);
    assert_eq!(authored.len(), 1);
    let mut replied = Snapshot::default();
    replied.record_comments("reviews", Ok(vec![("7".into(), "2 comments".into())]));
    previous.adopt_authored(&replied, &mut authored);
    assert!(authored.is_empty());
    assert!(previous.changes(&replied, "pr").is_empty());
}

#[test]
fn adopted_states_do_not_wake_even_before_a_first_observation() {
    let open = Snapshot {
        state: "open".into(),
        ..Default::default()
    };
    let mut closed = Snapshot {
        state: "closed".into(),
        ..Default::default()
    };
    let mut seen = open.clone();
    seen.adopt_state("closed");
    assert!(seen.changes(&closed, "pr").is_empty());
    closed.record_comments(DISCUSSION, Ok(vec![("1".into(), "theirs".into())]));
    let mut unseen = Snapshot::default();
    unseen.adopt_state("closed");
    let kinds: Vec<_> = unseen
        .changes(&closed, "pr")
        .iter()
        .map(|update| update.kind)
        .collect();
    assert_eq!(kinds, [UpdateKind::Comment]);
}

#[test]
fn issue_closure_and_reopening_have_item_messages() {
    let mut opened = Snapshot {
        state: "open".into(),
        ..Default::default()
    };
    let closed = Snapshot {
        state: "closed".into(),
        ..Default::default()
    };
    let updates = opened.changes(&closed, "https://forge.example/team/repo/issues/1");
    assert_eq!(updates[0].kind, UpdateKind::Closed);
    assert!(updates[0].message.starts_with("Issue"));
    assert_eq!(
        closed.changes(&opened, "issue")[0].kind,
        UpdateKind::Reopened
    );
    opened.state.clear();
    assert!(
        opened
            .changes(
                &Snapshot {
                    state: "open".into(),
                    ..Default::default()
                },
                "issue"
            )
            .is_empty()
    );
}

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
    snapshot.normalize(&rest_snapshot("rev-1", true, &[]), 1000);
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
    let before = rest_snapshot("rev-1", true, &[("review", "Success")]);
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let pull = json!({"number": 7, "title": "Fix", "state": "open", "mergeable": false,
            "head": {"ref": "topic", "sha": "rev-1"}, "base": {"ref": "main"}});
    let pull = forgejo.current(ItemKind::Pr, 7, &pull).unwrap();
    let mut failed = Snapshot::default();
    failed.record_checks(&pull, Err(anyhow::anyhow!("no status available")));
    failed
        .comments
        .insert("discussion:1".into(), "new finding".into());
    failed.normalize(&before, 1000);
    let updates = before.changes(&failed, "pr");
    assert_eq!(
        updates.iter().map(|u| u.kind).collect::<Vec<_>>(),
        [
            UpdateKind::Comment,
            UpdateKind::MergeConflict,
            UpdateKind::LookupFailed
        ]
    );
    assert!(updates[2].message.contains("no status available"));
    assert!(failed.changes(&failed, "pr").is_empty());
    let mut recovered = rest_snapshot("rev-1", true, &[("review", "Success")]);
    recovered
        .comments
        .insert("discussion:1".into(), "new finding".into());
    assert!(failed.changes(&recovered, "pr").is_empty());
    let mut unknown = Snapshot::default();
    unknown.normalize(&recovered, 1000);
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
        snapshot.normalize(previous, 1000);
        let kinds = previous
            .changes(&snapshot, "pr")
            .iter()
            .map(|update| update.kind)
            .collect::<Vec<_>>();
        (snapshot, kinds)
    };
    let (seen, _) = observe(&Snapshot::default(), "rebased", "main-1");
    let (before, kinds) = observe(&seen, "rebased", "main-1");
    assert_eq!(kinds, [UpdateKind::MergeConflict]);
    let (returned, kinds) = observe(&before, "rebased", "main-2");
    assert!(kinds.is_empty());
    let (_, kinds) = observe(&returned, "rebased", "main-2");
    assert_eq!(kinds, [UpdateKind::MergeConflict]);
}

#[test]
fn a_forgejo_push_waits_for_a_second_poll_to_report_a_conflict() {
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let observe = |previous: &Snapshot, head: &str, mergeable: bool| {
        let pull = json!({"number": 7, "title": "Fix", "state": "open", "mergeable": mergeable,
                "head": {"ref": "topic", "sha": head}, "base": {"ref": "main", "sha": "main-1"}});
        let pull = forgejo.current(ItemKind::Pr, 7, &pull).unwrap();
        let mut snapshot = Snapshot::default();
        snapshot.record_checks(&pull, Ok(Vec::new()));
        snapshot.normalize(previous, 1000);
        let kinds = previous
            .changes(&snapshot, "pr")
            .iter()
            .map(|update| update.kind)
            .collect::<Vec<_>>();
        (snapshot, kinds)
    };
    let (mergeable, _) = observe(&Snapshot::default(), "rev-1", true);
    // Forgejo shows a PR it is still checking as not mergeable.
    let (checking, kinds) = observe(&mergeable, "rev-2", false);
    assert!(kinds.is_empty());
    let (checked, kinds) = observe(&checking, "rev-2", true);
    assert!(kinds.is_empty());
    let (checking, kinds) = observe(&checked, "rev-3", false);
    assert!(kinds.is_empty());
    let (_, kinds) = observe(&checking, "rev-3", false);
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

#[test]
fn a_run_reports_once_when_it_finishes_with_its_failed_steps() {
    let url = "https://forge.example/team/repo/actions/runs/2765";
    let running = Snapshot::new("running");
    assert!(Snapshot::default().changes(&running, url).is_empty());
    let mut failed = Snapshot::new("failed");
    failed.failed_jobs = vec![
        FailedJob {
            name: "audit".into(),
            step: Some("Audit".into()),
        },
        FailedJob {
            name: "lint".into(),
            step: None,
        },
    ];
    let updates = running.changes(&failed, url);
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
    assert!(
        updates[0]
            .message
            .contains("audit (failed step: Audit), lint;"),
        "{}",
        updates[0].message
    );
    assert!(failed.run_finished());
    assert!(failed.changes(&failed, url).is_empty());
    let finished = Snapshot::new("finished");
    assert_eq!(Snapshot::default().changes(&finished, url).len(), 1);
}

#[test]
fn a_run_with_many_failed_jobs_reports_a_bounded_update() {
    let url = "https://forge.example/team/repo/actions/runs/2765";
    let mut failed = Snapshot::new("failed");
    failed.failed_jobs = (0..5000)
        .map(|index| FailedJob {
            name: format!("shard {index} {}", "x".repeat(1000)),
            step: Some("y".repeat(1000)),
        })
        .collect();
    let message = &Snapshot::new("running").changes(&failed, url)[0].message;
    assert!(message.len() < 4096, "{} bytes", message.len());
    assert!(message.contains("and 4990 more"), "{message}");
}

#[test]
fn head_checks_leave_out_failures_kept_from_an_earlier_head() {
    let mut snapshot = Snapshot {
        head: "2".into(),
        ..Snapshot::default()
    };
    for (key, revision) in [("old", "1"), ("new", "2")] {
        let check = Check {
            name: key.into(),
            state: "failure".into(),
            revision: revision.into(),
            complete: true,
        };
        snapshot.checks.insert(key.into(), check);
    }
    snapshot.errors.insert("CI".into(), "timed out".into());
    let checks: Vec<_> = snapshot.head_checks().collect();
    assert_eq!(checks, [("new", "failure", true)]);
    assert_eq!(snapshot.errors().collect::<Vec<_>>(), [("CI", "timed out")]);
}

#[test]
fn a_conflict_kept_from_an_earlier_head_or_base_is_unknown_at_the_new_one() {
    let mut snapshot = Snapshot {
        head: "1".into(),
        base: "main".into(),
        conflict: Some(true),
        conflict_at: MergeRevision {
            head: "1".into(),
            base: "main".into(),
        },
        ..Snapshot::default()
    };
    assert_eq!(snapshot.head_conflict(), Some(true));
    snapshot.base = "main-2".into();
    assert_eq!(snapshot.head_conflict(), None);
    snapshot.base = "main".into();
    snapshot.head = "2".into();
    assert_eq!(snapshot.head_conflict(), None);
}

#[test]
fn edits_to_an_items_details_are_reported_together() {
    let details = |details: serde_json::Value| -> Snapshot {
        serde_json::from_value(json!({"comments": {}, "checks": {}, "conflict": null,
                "state": "open", "errors": {}, "details": details}))
        .unwrap()
    };
    let before = details(
        json!({"title": "Fix", "body": "Steps", "labels": ["bug", "wip"],
            "assignees": [], "draft": true, "base": "main"}),
    );
    let after = details(json!({"title": "Fix the parser", "body": "Steps\n\nMore",
            "labels": ["bug", "docs"], "assignees": ["alice"], "milestone": "v1",
            "draft": false, "base": "release"}));
    let updates = before.changes(&after, "pr");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].kind, UpdateKind::Edited);
    assert_eq!(
        updates[0].message,
        "Title changed to \"Fix the parser\"; description edited; labels added: docs; \
         labels removed: wip; assignees added: alice; milestone set to v1; \
         marked ready for review; base branch changed to release"
    );
    assert!(after.changes(&after, "pr").is_empty());
    let mut cleared = after.clone();
    cleared.details.as_mut().unwrap().milestone = None;
    assert_eq!(
        after.changes(&cleared, "pr")[0].message,
        "Milestone removed"
    );
    // A snapshot saved before details were recorded takes them as the
    // baseline.
    let saved = details(serde_json::Value::Null);
    assert!(saved.details.is_none());
    assert!(saved.changes(&after, "pr").is_empty());
}

#[test]
fn own_edits_are_taken_as_seen_leaving_other_changes_reported() {
    let snapshot = |base: &str, labels: serde_json::Value| -> Snapshot {
        serde_json::from_value(json!({"comments": {}, "checks": {}, "conflict": null,
                "state": "open", "errors": {}, "details": {"title": "Fix", "body": "",
                "labels": labels, "assignees": [], "base": base}}))
        .unwrap()
    };
    let mut seen = snapshot("feature", json!([]));
    seen.adopt_edited(&Edited::retarget("feature", "main"));
    assert!(seen.changes(&snapshot("main", json!([])), "pr").is_empty());
    let labelled = seen.changes(&snapshot("main", json!(["bug"])), "pr");
    assert_eq!(labelled[0].message, "Labels added: bug");
    // A label someone else added before the agent added its own is still
    // reported, and one the agent removed is not.
    let labels = |names: serde_json::Value| snapshot("main", names).details.unwrap();
    let own = Edited {
        before: labels(json!(["bug", "wip"])),
        after: labels(json!(["bug", "docs", "wip"])),
    };
    let mut seen = snapshot("main", json!(["wip"]));
    seen.adopt_edited(&own);
    seen.adopt_edited(&Edited {
        before: labels(json!(["bug", "docs", "wip"])),
        after: labels(json!(["bug", "docs"])),
    });
    let updates = seen.changes(&snapshot("main", json!(["bug", "docs"])), "pr");
    assert_eq!(updates[0].message, "Labels added: bug");
    // An item not observed yet takes its details from its first observation.
    let mut unobserved = Snapshot::default();
    unobserved.adopt_edited(&Edited::retarget("feature", "main"));
    assert!(unobserved.details.is_none());
}

#[test]
fn a_merge_ends_activity_and_closing_ends_it_once_the_assignment_is_done() {
    let snapshot = |state: &str| Snapshot::new(state);
    assert!(snapshot("merged").concluded() && snapshot("closed").concluded());
    assert!(!snapshot("open").concluded());
    assert!(snapshot("merged").activity_ended(false));
    assert!(!snapshot("closed").activity_ended(false));
    assert!(snapshot("closed").activity_ended(true));
    assert!(!snapshot("open").activity_ended(true));
}
