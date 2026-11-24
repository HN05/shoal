use super::*;
use crate::daemon::{FindingCode, FindingThread};
use crate::reviews::{Severity, Verdict};
use crate::stand_in::StandInRecords;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::code::{CodeAt, Lines};
use shoal_protocol::item_numbers::ItemNumber;
use shoal_protocol::threads::Outcome;
use shoal_store::Store;

/// A migrated store with one workspace, `work`, in the stand-in's records.
async fn store(directory: &tempfile::TempDir) -> Store {
    crate::stand_in::store(directory.path().join("state.sqlite3"), "work").await
}

/// The records of one workspace, `work`, named `fix-login`, without threads.
fn records() -> StandInRecords {
    StandInRecords {
        workspace: "work",
        name: "fix-login",
        threads: Default::default(),
    }
}

/// The reopen of the finding `thread`, which a run holds until its submit.
struct Reopened {
    thread: ItemNumber,
    reason: String,
}

/// Write running review `id` as its run does, as `reviewer`: `findings`
/// findings in the commit `head`, the reopens `reopened` of those
/// `fixed`, then submit it, recording the `session` its run reported.
fn submit_with(
    tx: &rusqlite::Transaction<'_>,
    records: &StandInRecords,
    id: ReviewId,
    reviewer: &str,
    (findings, reopened): (u32, Vec<Reopened>),
    session: Option<&AgentSessionId>,
) -> Result<bool> {
    let workspace = SpaceId::from("work");
    attempt(tx, id, &reviewer.into())?;
    for line in 1..=findings {
        let code = FindingCode {
            commit: "head".into(),
            at: CodeAt {
                path: "src/a.rs".into(),
                lines: Some(Lines {
                    start: line - 1,
                    end: line + 1,
                }),
                blob: None,
            },
            snippet: None,
        };
        let finding = FindingThread {
            review: id,
            workspace: &workspace,
            severity: Severity::ShouldFix,
            title: &format!("Problem {line}"),
            body: "It breaks.",
            code: std::slice::from_ref(&code),
        };
        records.open_finding(tx, &finding)?;
    }
    for reopen in reopened {
        if let Some(thread) = records.fixed_finding(tx, &workspace, reopen.thread)? {
            hold_reopen(tx, id, thread, &reopen.reason)?;
        }
    }
    let now = UnixTime::from_seconds(3);
    let submitted = submit_run(
        tx,
        records,
        id,
        &workspace,
        Some("Two problems."),
        false,
        now,
    )?;
    if let Some(session) = session {
        continue_session(tx, id, session)?;
    }
    Ok(submitted)
}

/// Queue a review of `work` and start it at `head`.
fn started(tx: &rusqlite::Transaction<'_>, target: &ReviewTarget, head: &str) -> Result<ReviewId> {
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
        &head.into(),
        &"base".into(),
        UnixTime::from_seconds(2),
    )?;
    Ok(id)
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
            assert_eq!(load(db, &records(), first.id())?.unwrap().model(), Some(&model));
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
            assert!(requeue(db, first.id(), true)?);
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
async fn findings_open_threads_and_a_later_review_reopens_a_fix_still_present() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory).await;
    store
        .write(|tx| {
            let records = records();
            let first = started(tx, &ReviewTarget::Workspace, "one")?;
            assert!(submit_with(
                tx,
                &records,
                first,
                "claude",
                (2, Vec::new()),
                None
            )?);
            // A second submit finds it finished.
            assert!(!submit_with(
                tx,
                &records,
                first,
                "claude",
                (0, Vec::new()),
                None
            )?);
            let review = load(tx, &records, first)?.unwrap();
            assert_eq!(review.workspace, "fix-login");
            assert_eq!(review.state, ReviewState::Finished);
            assert_eq!(review.verdict, Some(Verdict::RequestChanges));
            assert!(review.threads.iter().all(|thread| !thread.held));
            let numbers: Vec<String> = (review.threads.iter())
                .map(|thread| thread.finding_line())
                .collect();
            assert_eq!(
                numbers,
                [
                    "1.1 should-fix at src/a.rs:1-2: Problem 1",
                    "1.2 should-fix at src/a.rs:2-3: Problem 2"
                ]
            );
            let [fixed, wrong] = [review.threads[0].number, review.threads[1].number];
            records.resolve(fixed, Some(Outcome::Fixed));
            let reason = "Checked".to_owned();
            records.resolve(wrong, Some(Outcome::FalsePositive { reason }));

            let second = started(tx, &ReviewTarget::Workspace, "two")?;
            let still = |thread| Reopened {
                thread,
                reason: "Still breaks.".into(),
            };
            // Only a thread resolved fixed reopens.
            let checked = (0, vec![still(fixed), still(wrong)]);
            assert!(submit_with(tx, &records, second, "claude", checked, None)?);
            let review = load(tx, &records, second)?.unwrap();
            let reopened: Vec<_> = review.reopened.iter().map(|thread| thread.number).collect();
            assert_eq!(reopened, [fixed]);
            assert_eq!(review.unresolved().count(), 1);

            let listed = list(tx, &records, &["work".into()])?;
            assert_eq!(
                listed.iter().map(|review| review.id).collect::<Vec<_>>(),
                [second, first]
            );
            let status = latest_status(tx, &records, &SpaceId::from("work"))?.unwrap();
            assert_eq!((status.id, status.unresolved), (second, 1));
            assert_eq!(
                earlier(tx, &records, &SpaceId::from("work"), second)?.len(),
                1
            );
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
            let review = load(db, &records(), id)?.unwrap();
            assert_eq!((review.state, review.head), (ReviewState::Queued, None));
            assert_eq!(queued(db)?, [id]);
            assert!(fail(
                db,
                id,
                Some(&"codex".into()),
                FailureReason::RateLimit,
                UnixTime::from_seconds(3)
            )?);
            let review = load(db, &records(), id)?.unwrap();
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
            let records = records();
            let first = started(tx, &ReviewTarget::Workspace, "one")?;
            submit_with(tx, &records, first, "claude", (1, Vec::new()), None)?;
            let fixed = records.threads.lock().unwrap()[0].number;
            records.resolve(fixed, Some(Outcome::Fixed));
            let second = started(tx, &ReviewTarget::Workspace, "two")?;
            let reopened = Reopened {
                thread: fixed,
                reason: "Still.".into(),
            };
            submit_with(tx, &records, second, "claude", (0, vec![reopened]), None)?;
            tx.execute("DELETE FROM spaces WHERE id='work'", [])?;
            let left: i64 = tx.query_row(
                "SELECT (SELECT COUNT(*) FROM reviews)+(SELECT COUNT(*) FROM review_reopens)",
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
                let id = started(tx, target, "head")?;
                let session = session.map(|session| AgentSessionId::from(session.to_owned()));
                let written = (0, Vec::new());
                submit_with(tx, &records(), id, reviewer, written, session.as_ref())?;
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
            // An answer in a finding's thread goes on with the latest
            // session, which later reviews continue.
            let (holder, agent, session) = latest_session(tx, last)?.unwrap();
            assert_eq!(
                (agent.to_string(), session.as_str()),
                ("codex".into(), "c1")
            );
            continue_session(tx, holder, &AgentSessionId::from("c2".to_owned()))?;
            let continued = review(&ReviewTarget::Workspace, "claude", None)?;
            assert_eq!(saved(continued)?, pair("codex", "c2"));
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
