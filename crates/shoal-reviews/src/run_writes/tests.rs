use shoal_protocol::{
    code::{Lines, NewCodeRef},
    space_names::SpaceSelector,
    threads::{NewPost, Outcome, ThreadNumber, ThreadOpening},
};

use crate::{
    reviews::{Review, ReviewState, Severity, Verdict},
    stand_in::Fixture,
    test_support::until_tests_end,
    tests::fixture,
};

/// A reviewer that says it started, once its run's checkout exists, then
/// runs until the tests end.
fn waits() -> String {
    format!(
        "touch \"$(dirname '{{review}}')/started\"\n{}",
        until_tests_end()
    )
}

impl Fixture {
    /// Queue a review by `agent` and return it once its reviewer started
    /// in its checkout.
    async fn running(&self, agent: &str) -> Review {
        let queued = self.queue(Some(agent.parse().unwrap())).await.unwrap();
        let started = self.written("started");
        while !started.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        self.shown(queued.id).await.unwrap()
    }

    /// The path of `path` in the checkout of the workspace's running review.
    fn in_checkout(&self, path: &str) -> std::path::PathBuf {
        self.run().join("checkout").join(path)
    }
}

/// A finding named `name` of `severity` about `path`'s first lines.
fn finding(name: &str, severity: Option<Severity>, path: std::path::PathBuf) -> ThreadOpening {
    ThreadOpening {
        body: Some("Name it.".into()),
        code: vec![NewCodeRef {
            path,
            lines: Some(Lines { start: 0, end: 9 }),
            contents: None,
        }],
        severity,
        ..ThreadOpening::named(name)
    }
}

#[tokio::test]
async fn a_run_opens_findings_on_the_reviewed_commit_and_submits_them_together() {
    let fixture = fixture(&[("slow", &waits())], "agent = 'slow'").await;
    let review = fixture.running("slow").await;
    let (reviews, daemon) = (&fixture.reviews, &*fixture.daemon);
    let open = |opening| reviews.open_run_finding(daemon, review.id, opening, None);
    let tracked = fixture.in_checkout("tracked");
    // A finding says how much it matters and points inside the checkout.
    assert!(
        open(finding("Unclear", None, tracked.clone()))
            .await
            .is_err()
    );
    let outside = fixture.workspace.path.join("tracked");
    let opened = open(finding("Unclear", Some(Severity::Nit), outside)).await;
    assert!(opened.is_err());
    let thread = open(finding(
        "Unclear",
        Some(Severity::ShouldFix),
        tracked.clone(),
    ))
    .await
    .unwrap();
    assert!(thread.held);
    let code = thread.about().unwrap();
    // The reviewed commit's lines, cut to its file's.
    assert_eq!(code.commit, review.head);
    assert_eq!(code.original.path, "tracked");
    assert_eq!(code.snippet.as_deref(), Some("initial"));
    // A comment towards another workspace is refused.
    let elsewhere = SpaceSelector::id(&"other".into());
    let towards = reviews.open_run_finding(
        daemon,
        review.id,
        finding("Unclear", Some(Severity::Nit), tracked.clone()),
        Some(&elsewhere),
    );
    assert!(towards.await.is_err());

    let submitted = reviews.submit_run(daemon, review.id, None, false, Some("One problem.".into()));
    let submitted = submitted.await.unwrap();
    assert_eq!(submitted.state, ReviewState::Finished);
    assert_eq!(submitted.verdict, Some(Verdict::RequestChanges));
    assert_eq!(submitted.head, review.head);
    assert!(submitted.threads.iter().all(|thread| !thread.held));
    let messages = fixture.messages();
    assert!(
        messages.iter().any(|message| message.contains("Unclear")),
        "{messages:?}"
    );
    // Once submitted, the run writes no more.
    let again = open(finding("Later", Some(Severity::Nit), tracked)).await;
    assert!(again.is_err());
}

#[tokio::test]
async fn a_runs_reopen_of_a_fixed_finding_waits_for_its_submit() {
    let fixture = fixture(
        &[("fake", "cat '{review}'"), ("slow", &waits())],
        "agent = 'fake'",
    )
    .await;
    let first = fixture.review().await;
    let number = first.threads[0].number;
    let (reviews, daemon) = (&fixture.reviews, &*fixture.daemon);
    let review = fixture.running("slow").await;
    let post = || NewPost {
        text: "The empty case still panics.".into(),
        ..NewPost::default()
    };
    // Only a finding resolved fixed reopens.
    let reopen = reviews.reopen_run_finding(daemon, review.id, ThreadNumber::Full(number), post());
    assert!(reopen.await.is_err());
    fixture
        .resolve(&first.threads[0], Some(Outcome::Fixed))
        .await
        .unwrap();
    let reopen = reviews.reopen_run_finding(daemon, review.id, ThreadNumber::Full(number), post());
    assert!(reopen.await.unwrap().resolution.is_some());
    let submitted = reviews.submit_run(daemon, review.id, None, false, None);
    let submitted = submitted.await.unwrap();
    let reopened: Vec<_> = (submitted.reopened.iter())
        .map(|thread| thread.number)
        .collect();
    assert_eq!(reopened, [number]);
    assert!(submitted.reopened[0].resolution.is_none());
    assert_eq!(submitted.verdict, Some(Verdict::Comment));
}
