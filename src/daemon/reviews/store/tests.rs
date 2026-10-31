use super::*;
use crate::{
    daemon::store::Store,
    reviews::{FixCheck, OutcomeKind, Severity, Verdict, output::NewFinding},
};

/// A migrated store with one workspace, `work`.
async fn store(directory: &tempfile::TempDir) -> Store {
    let store = Store::open(directory.path().join("state.sqlite3"))
        .await
        .unwrap();
    store
        .run(|db| {
            db.execute_batch(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                INSERT INTO workspaces(id,repository_id,name,path,branch,state)
                    VALUES ('work','repo','fix-login','/work','fix-login','ready');",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    store
}

fn output(findings: usize, checked: Vec<Checked>) -> ReviewerOutput {
    ReviewerOutput {
        summary: "Two problems.".into(),
        verdict: Verdict::Comment,
        findings: (1..=findings)
            .map(|line| NewFinding {
                path: "src/a.rs".into(),
                line: line as u32,
                severity: Severity::ShouldFix,
                title: format!("Problem {line}"),
                body: "It breaks.".into(),
            })
            .collect(),
        checked,
    }
}

#[tokio::test]
async fn one_review_is_queued_or_running_per_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory).await;
    store
        .run(|db| {
            let first = queue(db, "work", &ReviewTarget::Workspace, "claude", 1)?;
            assert!(matches!(first, Queued::New(_)));
            let again = queue(db, "work", &ReviewTarget::Workspace, "codex", 2)?;
            assert_eq!(again, Queued::Existing(first.id()));
            assert!(start(db, first.id(), "head", "base", 3)?);
            let refused = queue(db, "work", &ReviewTarget::Workspace, "claude", 4).unwrap_err();
            assert!(
                refused.to_string().contains("shoal review cancel"),
                "{refused}"
            );
            assert!(cancel(db, first.id(), Some("work"), 5)?);
            assert_eq!(cancelled_by(db, first.id())?.as_deref(), Some("work"));
            assert!(!cancel(db, first.id(), None, 6)?);
            assert!(matches!(
                queue(db, "work", &ReviewTarget::Workspace, "claude", 7)?,
                Queued::New(id) if id > first.id()
            ));
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn findings_are_numbered_answered_and_checked_by_a_later_review() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory).await;
    store
        .run(|db| {
            let first = queue(db, "work", &ReviewTarget::Workspace, "claude", 1)?.id();
            start(db, first, "one", "base", 2)?;
            assert!(finish(db, first, "claude", &output(2, Vec::new()), 3)?);
            let finding = FindingId {
                review: first,
                number: 1,
            };
            let other = FindingId {
                review: first,
                number: 2,
            };
            reply(db, finding, "Is this reachable?", 4)?;
            resolve(db, finding, Some(&Outcome::Fixed))?;
            let duplicate = Outcome::new(OutcomeKind::Duplicate, None, None, Some(finding))?;
            resolve(db, other, Some(&duplicate))?;
            assert!(resolve(db, finding, Some(&Outcome::Duplicate { of: finding })).is_err());
            assert!(
                reply(
                    db,
                    FindingId {
                        review: first,
                        number: 9
                    },
                    "x",
                    5
                )
                .is_err()
            );

            let second = queue(db, "work", &ReviewTarget::Workspace, "codex", 6)?.id();
            start(db, second, "two", "base", 7)?;
            let checked = vec![
                Checked {
                    finding,
                    result: FixCheck::Confirmed,
                },
                // Not resolved fixed, so not checked.
                Checked {
                    finding: other,
                    result: FixCheck::StillPresent,
                },
            ];
            assert!(finish(db, second, "claude", &output(0, checked), 8)?);

            let review = load(db, first)?.unwrap();
            assert_eq!(review.workspace, "fix-login");
            assert_eq!(review.state, ReviewState::Finished);
            assert_eq!(review.duration(), Some(1));
            assert_eq!(review.findings[0].replies[0].body, "Is this reachable?");
            assert_eq!(
                review.findings[0].check,
                Some(Check {
                    result: FixCheck::Confirmed,
                    review: second
                })
            );
            assert_eq!(review.findings[1].outcome, Some(duplicate));
            assert_eq!(review.findings[1].check, None);
            assert_eq!(review.unresolved().count(), 0);

            let listed = list(db, &["work".into()])?;
            assert_eq!(
                listed.iter().map(|review| review.id).collect::<Vec<_>>(),
                [second, first]
            );
            assert_eq!(latest(db, "work")?.unwrap().id, second);
            assert_eq!(earlier(db, "work", second)?.len(), 1);

            resolve(db, finding, None)?;
            assert_eq!(load(db, first)?.unwrap().unresolved().count(), 1);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn failures_are_typed_and_interrupted_runs_start_over() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory).await;
    store
        .run(|db| {
            let id = queue(db, "work", &ReviewTarget::Workspace, "claude", 1)?.id();
            start(db, id, "head", "base", 2)?;
            assert_eq!(requeue_running(db)?, 1);
            let review = load(db, id)?.unwrap();
            assert_eq!((review.state, review.head), (ReviewState::Queued, None));
            assert_eq!(queued(db)?, [id]);
            assert!(fail(db, id, Some("codex"), FailureReason::RateLimit, 3)?);
            let review = load(db, id)?.unwrap();
            assert_eq!(review.failure, Some(FailureReason::RateLimit));
            assert_eq!(review.status(), "failed: rate-limit");
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn removing_the_workspace_deletes_its_reviews() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory).await;
    store
        .run(|db| {
            let id = queue(db, "work", &ReviewTarget::Workspace, "claude", 1)?.id();
            start(db, id, "head", "base", 2)?;
            finish(db, id, "claude", &output(1, Vec::new()), 3)?;
            reply(
                db,
                FindingId {
                    review: id,
                    number: 1,
                },
                "Why?",
                4,
            )?;
            db.execute("DELETE FROM workspaces WHERE id='work'", [])?;
            let left: i64 = db.query_row(
                "SELECT (SELECT COUNT(*) FROM reviews)+(SELECT COUNT(*) FROM review_findings)
                    +(SELECT COUNT(*) FROM review_replies)",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(left, 0);
            Ok(())
        })
        .await
        .unwrap();
}
