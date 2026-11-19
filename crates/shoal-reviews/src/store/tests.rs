use super::*;
use crate::reviews::{FixCheck, OutcomeKind, Severity, Verdict, output::NewFinding};
use shoal_ids::space_ids::SpaceId;
use shoal_store::Store;

/// A migrated store with one workspace, `work`, in the stand-in's records.
async fn store(directory: &tempfile::TempDir) -> Store {
    crate::stand_in::store(directory.path().join("state.sqlite3"), "work").await
}

/// The records of one workspace, `work`, named `fix-login`.
const RECORDS: &crate::stand_in::StandInRecords = &crate::stand_in::StandInRecords {
    workspace: "work",
    name: "fix-login",
};

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
            let first = queue(db, &SpaceId::from("work"), &ReviewTarget::Workspace, &"claude".into(), &model, UnixTime::from_seconds(1))?;
            assert!(matches!(first, Queued::New(_)));
            assert_eq!(load(db, RECORDS, first.id())?.unwrap().model, model);
            let again = queue(
                db,
                &SpaceId::from("work"),
                &ReviewTarget::Workspace,
                &"codex".into(),
                &Default::default(),
                UnixTime::from_seconds(2),
            )?;
            assert_eq!(again, Queued::Existing(first.id()));
            assert!(start(db, first.id(), &"head".into(), &"base".into(), UnixTime::from_seconds(3))?);
            // A run protection stopped waits for healthy readings until the
            // review starts again.
            assert!(requeue(db, first.id())?);
            assert!(waits_for_recovery(db, first.id())?);
            assert!(start(db, first.id(), &"head".into(), &"base".into(), UnixTime::from_seconds(3))?);
            assert!(!waits_for_recovery(db, first.id())?);
            let refused = queue(
                db,
                &SpaceId::from("work"),
                &ReviewTarget::Workspace,
                &"claude".into(),
                &Default::default(),
                UnixTime::from_seconds(4),
            );
            assert!(refused.is_err());
            assert!(cancel(db, first.id(), Some(&SpaceId::from("work")), UnixTime::from_seconds(5))?);
            assert_eq!(cancelled_by(db, first.id())?.as_deref(), Some("work"));
            assert!(!cancel(db, first.id(), None, UnixTime::from_seconds(6))?);
            assert!(matches!(
                queue(db, &SpaceId::from("work"), &ReviewTarget::Workspace, &"claude".into(), &Default::default(), UnixTime::from_seconds(7))?,
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
                &SpaceId::from("work"),
                &ReviewTarget::Workspace,
                &"claude".into(),
                &Default::default(),
                UnixTime::from_seconds(1),
            )?
            .id();
            start(
                tx,
                first,
                &"one".into(),
                &"base".into(),
                UnixTime::from_seconds(2),
            )?;
            assert!(finish(
                tx,
                first,
                &"claude".into(),
                &output(2, Vec::new()),
                None,
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
                &SpaceId::from("work"),
                &ReviewTarget::Workspace,
                &"codex".into(),
                &Default::default(),
                UnixTime::from_seconds(6),
            )?
            .id();
            start(
                tx,
                second,
                &"two".into(),
                &"base".into(),
                UnixTime::from_seconds(7),
            )?;
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
                &"claude".into(),
                &output(0, checked),
                None,
                UnixTime::from_seconds(8)
            )?);

            let review = load(tx, RECORDS, first)?.unwrap();
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

            let listed = list(tx, RECORDS, &["work".into()])?;
            assert_eq!(
                listed.iter().map(|review| review.id).collect::<Vec<_>>(),
                [second, first]
            );
            assert_eq!(
                latest(tx, RECORDS, &SpaceId::from("work"))?.unwrap().id,
                second
            );
            let status = latest_status(tx, RECORDS, &SpaceId::from("work"))?.unwrap();
            assert_eq!((status.id, status.unresolved), (second, 0));
            assert_eq!(
                earlier(tx, RECORDS, &SpaceId::from("work"), second)?.len(),
                1
            );

            resolve(tx, finding, None)?;
            assert_eq!(load(tx, RECORDS, first)?.unwrap().unresolved().count(), 1);
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
                &SpaceId::from("work"),
                &ReviewTarget::Workspace,
                &"claude".into(),
                &Default::default(),
                UnixTime::from_seconds(1),
            )?
            .id();
            start(
                db,
                id,
                &"head".into(),
                &"base".into(),
                UnixTime::from_seconds(2),
            )?;
            assert_eq!(requeue_running(db, &[])?, 1);
            let review = load(db, RECORDS, id)?.unwrap();
            assert_eq!((review.state, review.head), (ReviewState::Queued, None));
            assert_eq!(queued(db)?, [id]);
            assert!(fail(
                db,
                id,
                Some(&"codex".into()),
                FailureReason::RateLimit,
                UnixTime::from_seconds(3)
            )?);
            let review = load(db, RECORDS, id)?.unwrap();
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
                &SpaceId::from("work"),
                &ReviewTarget::Workspace,
                &"claude".into(),
                &Default::default(),
                UnixTime::from_seconds(1),
            )?
            .id();
            start(
                tx,
                id,
                &"head".into(),
                &"base".into(),
                UnixTime::from_seconds(2),
            )?;
            finish(
                tx,
                id,
                &"claude".into(),
                &output(1, Vec::new()),
                None,
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
            tx.execute("DELETE FROM spaces WHERE id='work'", [])?;
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

#[tokio::test]
async fn a_review_resumes_the_latest_session_of_its_workspace_or_pr() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory).await;
    let pr = |number| ReviewTarget::Pr {
        url: format!("https://forge.example/team/repo/pulls/{number}")
            .parse()
            .unwrap(),
        head: "topic".into(),
        base: "main".into(),
        post: false,
    };
    store
        .write(move |tx| {
            // Each review finishes before the next is queued, as one runs at
            // a time, by `reviewer` with the session it reported.
            let review = |target: &ReviewTarget, reviewer: &str, session: Option<&str>| {
                let id = queue(
                    tx,
                    &SpaceId::from("work"),
                    target,
                    &"claude".into(),
                    &Default::default(),
                    UnixTime::from_seconds(1),
                )?
                .id();
                start(
                    tx,
                    id,
                    &"head".into(),
                    &"base".into(),
                    UnixTime::from_seconds(2),
                )?;
                let session = session.map(|session| AgentSessionId::from(session.to_owned()));
                let output = output(0, Vec::new());
                let now = UnixTime::from_seconds(3);
                finish(tx, id, &reviewer.into(), &output, session.as_ref(), now)?;
                Ok::<_, anyhow::Error>(id)
            };
            let saved = |id| -> Result<Option<(String, String)>> {
                Ok(session(tx, id)?
                    .map(|(agent, session)| (agent.to_string(), session.as_str().to_owned())))
            };
            let pair = |agent: &str, session: &str| Some((agent.to_owned(), session.to_owned()));
            let first = review(&ReviewTarget::Workspace, "claude", Some("s1"))?;
            assert_eq!(saved(first)?, None);
            review(&pr(8), "claude", Some("p8"))?;
            // A run that reported none keeps the session before it.
            review(&ReviewTarget::Workspace, "claude", None)?;
            let next = review(&ReviewTarget::Workspace, "claude", Some("s2"))?;
            assert_eq!(saved(next)?, pair("claude", "s1"));
            // A fallback's session is the latest.
            let after = review(&ReviewTarget::Workspace, "codex", Some("c1"))?;
            assert_eq!(saved(after)?, pair("claude", "s2"));
            let last = review(&ReviewTarget::Workspace, "claude", None)?;
            assert_eq!(saved(last)?, pair("codex", "c1"));
            // Each PR keeps its own.
            let again = review(&pr(8), "claude", None)?;
            assert_eq!(saved(again)?, pair("claude", "p8"));
            let other = review(&pr(9), "claude", None)?;
            assert_eq!(saved(other)?, None);
            Ok(())
        })
        .await
        .unwrap();
}
