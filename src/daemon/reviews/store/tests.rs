use super::*;
use crate::time::UnixTime;
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
            let model = crate::agent::ModelChoice {
                model: Some("opus".into()),
                effort: None,
            };
            let first = queue(db, "work", &ReviewTarget::Workspace, "claude", &model, UnixTime::from_seconds(1))?;
            assert!(matches!(first, Queued::New(_)));
            assert_eq!(load(db, first.id())?.unwrap().model, model);
            let again = queue(
                db,
                "work",
                &ReviewTarget::Workspace,
                "codex",
                &Default::default(),
                UnixTime::from_seconds(2),
            )?;
            assert_eq!(again, Queued::Existing(first.id()));
            assert!(start(db, first.id(), "head", "base", UnixTime::from_seconds(3))?);
            // A run protection stopped waits for healthy readings until the
            // review starts again.
            assert!(requeue(db, first.id())?);
            assert!(waits_for_recovery(db, first.id())?);
            assert!(start(db, first.id(), "head", "base", UnixTime::from_seconds(3))?);
            assert!(!waits_for_recovery(db, first.id())?);
            let refused = queue(
                db,
                "work",
                &ReviewTarget::Workspace,
                "claude",
                &Default::default(),
                UnixTime::from_seconds(4),
            );
            assert!(refused.is_err());
            assert!(cancel(db, first.id(), Some("work"), UnixTime::from_seconds(5))?);
            assert_eq!(cancelled_by(db, first.id())?.as_deref(), Some("work"));
            assert!(!cancel(db, first.id(), None, UnixTime::from_seconds(6))?);
            assert!(matches!(
                queue(db, "work", &ReviewTarget::Workspace, "claude", &Default::default(), UnixTime::from_seconds(7))?,
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
        .write(|tx| {
            let first = queue(
                tx,
                "work",
                &ReviewTarget::Workspace,
                "claude",
                &Default::default(),
                UnixTime::from_seconds(1),
            )?
            .id();
            start(tx, first, "one", "base", UnixTime::from_seconds(2))?;
            assert!(finish(
                tx,
                first,
                "claude",
                &output(2, Vec::new()),
                UnixTime::from_seconds(3)
            )?);
            let finding = FindingId {
                review: first,
                number: 1,
            };
            let other = FindingId {
                review: first,
                number: 2,
            };
            reply(tx, finding, "Is this reachable?", UnixTime::from_seconds(4))?;
            resolve(tx, finding, Some(&Outcome::Fixed))?;
            let duplicate = Outcome::new(OutcomeKind::Duplicate, None, None, Some(finding))?;
            resolve(tx, other, Some(&duplicate))?;
            assert!(resolve(tx, finding, Some(&Outcome::Duplicate { of: finding })).is_err());
            assert!(
                reply(
                    tx,
                    FindingId {
                        review: first,
                        number: 9
                    },
                    "x",
                    UnixTime::from_seconds(5)
                )
                .is_err()
            );

            let second = queue(
                tx,
                "work",
                &ReviewTarget::Workspace,
                "codex",
                &Default::default(),
                UnixTime::from_seconds(6),
            )?
            .id();
            start(tx, second, "two", "base", UnixTime::from_seconds(7))?;
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
            assert!(finish(
                tx,
                second,
                "claude",
                &output(0, checked),
                UnixTime::from_seconds(8)
            )?);

            let review = load(tx, first)?.unwrap();
            assert_eq!(review.workspace, "fix-login");
            assert_eq!(review.state, ReviewState::Finished);
            assert_eq!(review.duration(), Some(std::time::Duration::from_secs(1)));
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

            let listed = list(tx, &["work".into()])?;
            assert_eq!(
                listed.iter().map(|review| review.id).collect::<Vec<_>>(),
                [second, first]
            );
            assert_eq!(latest(tx, "work")?.unwrap().id, second);
            let status = latest_status(tx, "work")?.unwrap();
            assert_eq!((status.id, status.unresolved), (second, 0));
            assert_eq!(earlier(tx, "work", second)?.len(), 1);

            resolve(tx, finding, None)?;
            assert_eq!(load(tx, first)?.unwrap().unresolved().count(), 1);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_review_reads_its_replies_without_scanning_others() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory).await;
    store
        .run(|db| {
            let plan = db
                .prepare(&format!("EXPLAIN QUERY PLAN {REPLIES}"))?
                .query_map([1], |row| row.get::<_, String>(3))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .join("; ");
            assert!(
                plan.contains("SEARCH review_replies USING INDEX review_replies_review"),
                "{plan}"
            );
            assert!(!plan.contains("TEMP B-TREE"), "{plan}");
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
            let id = queue(
                db,
                "work",
                &ReviewTarget::Workspace,
                "claude",
                &Default::default(),
                UnixTime::from_seconds(1),
            )?
            .id();
            start(db, id, "head", "base", UnixTime::from_seconds(2))?;
            assert_eq!(requeue_running(db, &[])?, 1);
            let review = load(db, id)?.unwrap();
            assert_eq!((review.state, review.head), (ReviewState::Queued, None));
            assert_eq!(queued(db)?, [id]);
            assert!(fail(
                db,
                id,
                Some("codex"),
                FailureReason::RateLimit,
                UnixTime::from_seconds(3)
            )?);
            let review = load(db, id)?.unwrap();
            assert_eq!(review.failure, Some(FailureReason::RateLimit));
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
        .write(|tx| {
            let id = queue(
                tx,
                "work",
                &ReviewTarget::Workspace,
                "claude",
                &Default::default(),
                UnixTime::from_seconds(1),
            )?
            .id();
            start(tx, id, "head", "base", UnixTime::from_seconds(2))?;
            finish(
                tx,
                id,
                "claude",
                &output(1, Vec::new()),
                UnixTime::from_seconds(3),
            )?;
            reply(
                tx,
                FindingId {
                    review: id,
                    number: 1,
                },
                "Why?",
                UnixTime::from_seconds(4),
            )?;
            tx.execute("DELETE FROM workspaces WHERE id='work'", [])?;
            let left: i64 = tx.query_row(
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
