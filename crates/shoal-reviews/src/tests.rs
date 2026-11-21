use std::path::PathBuf;

use shoal_ids::space_ids::SpaceId;

use crate::{
    checkouts::HeldCheckout,
    required::Gate,
    reviews::{FailureReason, Review, ReviewId, ReviewState, ReviewTarget, Severity},
    stand_in::Fixture,
    store,
    test_support::{install_test_script, until_tests_end},
    time::UnixTime,
};
use shoal_protocol::threads::{Outcome, Thread};

const REVIEW: &str = r#"{"summary":"One problem.","verdict":"comment","reopened":[],
"findings":[{"path":"tracked","start_line":1,"end_line":3,"severity":"should-fix",
"title":"Unclear","body":"Name it."}]}"#;

/// A workspace whose reviewers are `[agents.<name>]` scripts, in which
/// `{review}` names a file holding [`REVIEW`], with the review scheduler
/// running when there are any.
async fn fixture(reviewers: &[(&str, &str)], review: &str) -> Fixture {
    fixture_with(reviewers, review, "").await
}

/// [`fixture`] with `settings` in each reviewer's table.
async fn fixture_with(reviewers: &[(&str, &str)], review: &str, settings: &str) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let result = root.path().join("review.json");
    std::fs::write(&result, REVIEW).unwrap();
    let mut config = format!("[review]\n{review}\n");
    for (name, script) in reviewers {
        let path = root.path().join(format!("{name}.sh"));
        let script = script.replace("{review}", &result.to_string_lossy());
        install_test_script(&path, format!("#!/bin/sh\n{script}\n"));
        config.push_str(&format!(
            "[agents.{name}]\ncommand = ['{}']\nreview = ['{{prompt}}', '{{schema}}']\n{settings}",
            path.display()
        ));
    }
    let fixture = Fixture::new(root, &config).await;
    if !reviewers.is_empty() {
        fixture.run_reviews();
    }
    fixture
}

impl Fixture {
    /// What a reviewer script wrote beside [`REVIEW`] as `name`.
    fn written(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// The directory of the runs of the workspace's reviews.
    fn run(&self) -> PathBuf {
        self.runs.root().join(self.workspace.id.as_str())
    }

    /// The workspace's run, as review `id` left it, without a checkout.
    fn left_run(&self, id: ReviewId) -> PathBuf {
        self.runs.create().unwrap();
        let run = self.run();
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("review"), id.to_string()).unwrap();
        run
    }

    /// A review of the workspace, once its run ended and delivered it.
    async fn review(&self) -> Review {
        self.review_by(None).await
    }

    /// [`Self::review`] by `agent`, else the configured reviewer.
    async fn review_by(&self, agent: Option<&str>) -> Review {
        let mut ended = self.reviews.ends();
        let queued = self
            .queue(agent.map(|agent| agent.parse().unwrap()))
            .await
            .unwrap();
        ended.changed().await.unwrap();
        self.reviews.wait(&*self.daemon, queued.id).await.unwrap()
    }

    /// Queue a review of the workspace by `agent`.
    async fn queue(&self, agent: Option<crate::agent::Agent>) -> anyhow::Result<Review> {
        let target = ReviewTarget::Workspace;
        (self.reviews.queue(
            &*self.daemon,
            &self.selector(),
            target,
            agent,
            Default::default(),
        ))
        .await
    }

    /// Review `id` as requests show it.
    async fn shown(&self, id: ReviewId) -> anyhow::Result<Review> {
        self.reviews.review(&*self.daemon, id).await
    }

    /// Wait until review `id` is in `state`.
    async fn until(&self, id: ReviewId, state: ReviewState) {
        let mut changed = self.reviews.store.watch_changes();
        while self.shown(id).await.unwrap().state != state {
            changed.changed().await.unwrap();
        }
    }

    /// Resolve the finding's thread `thread` with `outcome`, or reopen it
    /// without one, as the daemon does: its false positives first.
    async fn resolve(&self, thread: &Thread, outcome: Option<Outcome>) -> anyhow::Result<()> {
        let kept = self
            .reviews
            .keep_false_positive(&*self.daemon, thread, outcome.as_ref());
        let _kept = kept.await?;
        self.records.resolve(thread.number, outcome);
        Ok(())
    }

    fn messages(&self) -> Vec<String> {
        self.daemon.messages.lock().unwrap().clone()
    }

    /// Record a review of the workspace that started running and left its
    /// run behind, as a stopped daemon's.
    async fn started(&self) -> ReviewId {
        let id = self.workspace.id.clone();
        self.reviews
            .store
            .run(move |db| {
                let review = queue_absent(db, &id)?;
                store::start(
                    db,
                    review,
                    &"head".into(),
                    &"base".into(),
                    UnixTime::from_seconds(2),
                )?;
                Ok(review)
            })
            .await
            .unwrap()
    }

    /// The path of the repository's false positives.
    fn false_positives(&self) -> PathBuf {
        crate::config::repo::false_positives_path(&self.repository.workspaces_dir)
    }
}

/// Queue a review of the workspace `id` by an agent no config names.
fn queue_absent(db: &rusqlite::Connection, id: &SpaceId) -> anyhow::Result<ReviewId> {
    let queued = store::queue(
        db,
        id,
        &ReviewTarget::Workspace,
        &"absent".into(),
        &Default::default(),
        UnixTime::from_seconds(1),
    )?;
    Ok(queued.id())
}

/// What the reviewer scripts write, beside [`REVIEW`].
fn written(fixture: &Fixture, name: &str) -> String {
    std::fs::read_to_string(fixture.written(name)).unwrap()
}

#[tokio::test]
async fn a_reviewer_runs_with_the_requested_or_configured_model_and_effort() {
    let script = "printf '%s\\n' \"$@\" > \"$(dirname '{review}')/args\"\ncat '{review}'";
    let profile = "agent = 'modeled'\n[agents.modeled]\nbase = 'fake'\neffort = 'low'\n\
        model_args = ['--model', '{model}']\neffort_args = ['--effort', '{effort}']\n";
    let fixture = fixture(&[("fake", script)], profile).await;
    let mut ended = fixture.reviews.ends();
    let model = crate::agent::ModelChoice {
        model: Some("m1".into()),
        effort: None,
    };
    let target = ReviewTarget::Workspace;
    let queued = (fixture.reviews)
        .queue(
            &*fixture.daemon,
            &fixture.selector(),
            target,
            None,
            model.clone(),
        )
        .await
        .unwrap();
    assert_eq!(queued.model, model);
    ended.changed().await.unwrap();
    let review = fixture
        .reviews
        .wait(&*fixture.daemon, queued.id)
        .await
        .unwrap();
    assert_eq!(review.state, ReviewState::Finished, "{review:?}");
    let args = written(&fixture, "args");
    // The selection comes before the review template's arguments.
    assert!(args.starts_with("--model\nm1\n--effort\nlow\n"), "{args}");
}

#[tokio::test]
async fn a_review_reads_the_committed_head_and_stores_its_findings() {
    // Uncommitted edits stay out of the checkout.
    let script = "[ -e uncommitted ] && exit 3\ncat '{review}'";
    let fixture = fixture(&[("fake", script)], "agent = 'fake'").await;
    std::fs::write(fixture.workspace.path.join("uncommitted"), "edit").unwrap();
    let review = fixture.review().await;
    assert_eq!(review.state, ReviewState::Finished, "{review:?}");
    assert_eq!(
        review.reviewer.as_ref().map(|agent| agent.name()),
        Some("fake")
    );
    let head = crate::git::run(&fixture.workspace.path, &["rev-parse", "HEAD"])
        .await
        .unwrap();
    assert_eq!(review.head, Some(head.trim().into()));
    assert!(review.base.is_some());
    assert_eq!(review.outdated, Some(false));
    let thread = &review.threads[0];
    assert_eq!(
        thread.finding_line(),
        "1.1 should-fix at tracked:1: Unclear"
    );
    assert_eq!(thread.finding.unwrap().severity, Severity::ShouldFix);
    let code = thread.about().unwrap();
    // The reviewer's lines are cut to the file's, whose version is kept.
    assert_eq!(
        (code.commit.as_ref(), code.snippet.as_deref()),
        (review.head.as_ref(), Some("initial"))
    );
    assert!(code.original.blob.is_some());
    assert!(!fixture.run().exists());
    let worktrees = crate::git::run(&fixture.workspace.path, &["worktree", "list"])
        .await
        .unwrap();
    assert!(!worktrees.contains("reviews"), "{worktrees}");
}

#[tokio::test]
async fn a_failed_reviewer_falls_back_once_and_failures_are_typed() {
    let fixture = fixture(
        &[
            ("first", "echo 'Error: 429 Too Many Requests' >&2\nexit 1"),
            ("second", "echo not json"),
        ],
        "agent = 'first'",
    )
    .await;
    let review = fixture.review().await;
    assert_eq!(review.state, ReviewState::Failed);
    assert_eq!(review.failure, Some(FailureReason::RateLimit));
    assert_eq!(
        review.reviewer.as_ref().map(|agent| agent.name()),
        Some("first")
    );

    fixture.configure("[review]\nagent = 'first'\nfallback_agent = 'second'\n[agents.first]\ncommand = ['false']\nreview = ['{prompt}', '{schema}']\n[agents.second]\ncommand = ['echo']\nreview = ['{prompt}', '{schema}']\n");
    let review = fixture.review().await;
    assert_eq!(review.state, ReviewState::Failed);
    assert_eq!(review.failure, Some(FailureReason::InvalidOutput));
    assert_eq!(
        (
            review.agent.name(),
            review.reviewer.as_ref().map(|agent| agent.name())
        ),
        ("first", Some("second"))
    );
}

#[tokio::test]
async fn a_fallback_reviewer_produces_the_review() {
    let fixture = fixture(
        &[
            ("first", "echo 'You have hit your usage limit' >&2\nexit 1"),
            ("second", "cat '{review}'"),
        ],
        "agent = 'first'\nfallback_agent = 'second'",
    )
    .await;
    let review = fixture.review().await;
    assert_eq!(review.state, ReviewState::Finished, "{review:?}");
    assert_eq!(
        review.reviewer.as_ref().map(|agent| agent.name()),
        Some("second")
    );
}

#[tokio::test]
async fn a_review_whose_admission_is_taken_back_is_queued_again() {
    let fixture = fixture(&[("slow", &until_tests_end())], "agent = 'slow'").await;
    let queued = fixture.queue(None).await.unwrap();
    fixture.until(queued.id, ReviewState::Running).await;
    let stop = fixture.daemon.stops.lock().unwrap().pop().unwrap();
    stop();
    fixture.until(queued.id, ReviewState::Queued).await;
    let review = fixture.shown(queued.id).await.unwrap();
    assert_eq!((review.head, review.started_at), (None, None));
    // A restarted daemon keeps it waiting for healthy readings too.
    let id = queued.id;
    let waits = (fixture.reviews.store).run(move |db| store::waits_for_recovery(db, id));
    assert!(waits.await.unwrap());
    // Once load recovers, it runs again.
    fixture.daemon.recovered.send_replace(true);
    fixture.until(queued.id, ReviewState::Running).await;
    let mut ended = fixture.reviews.ends();
    let cancelled = (fixture.reviews)
        .cancel(&*fixture.daemon, None, Some(queued.id), None)
        .await
        .unwrap();
    assert_eq!(cancelled.state, ReviewState::Cancelled);
    ended.changed().await.unwrap();
}

#[tokio::test]
async fn cancelling_stops_the_reviewer_and_removes_its_checkout() {
    let fixture = fixture(&[("slow", &until_tests_end())], "agent = 'slow'").await;
    let queued = fixture.queue(None).await.unwrap();
    fixture.until(queued.id, ReviewState::Running).await;
    let mut ended = fixture.reviews.ends();
    let by = Some(fixture.workspace.id.clone());
    let cancelled = (fixture.reviews)
        .cancel(&*fixture.daemon, Some(&fixture.selector()), None, by)
        .await
        .unwrap();
    assert_eq!(cancelled.state, ReviewState::Cancelled);
    ended.changed().await.unwrap();
    assert!(!fixture.run().exists());
    let again = (fixture.reviews)
        .cancel(&*fixture.daemon, None, Some(queued.id), None)
        .await;
    assert!(again.is_err());
}

#[tokio::test]
async fn a_reviewer_needs_a_review_template() {
    let fixture = fixture(&[], "").await;
    let refused = fixture.queue(Some("pi".parse().unwrap())).await;
    assert!(refused.is_err());
    let unset = fixture.queue(None).await;
    assert!(unset.is_err());
}

#[tokio::test]
async fn startup_removes_interrupted_checkouts_and_queues_their_reviews_again() {
    let fixture = fixture(&[], "").await;
    let queued = fixture.started().await;
    let checkout = fixture.left_run(queued).join("checkout");
    let head = crate::git::run(&fixture.workspace.path, &["rev-parse", "HEAD"])
        .await
        .unwrap();
    crate::git::detached::add(&fixture.workspace.path, &checkout, head.trim())
        .await
        .unwrap();
    // Reviewer processes that outlived their daemon, in the checkout and
    // after changing directory, and someone's shell in the checkout, none a
    // child of this process.
    let detached = |marked: bool, directory: &std::path::Path| {
        // This test binary, whose environment can be read, unlike that of
        // the platform's own executables on macOS.
        let mut command = std::process::Command::new("sh");
        command
            .args([
                "-c",
                "\"$0\" --exact tests::sleep_as_a_survivor --ignored >/dev/null 2>&1 & echo $!",
            ])
            .arg(std::env::current_exe().unwrap())
            .current_dir(directory);
        command.env(SURVIVOR_OWNER, std::process::id().to_string());
        if marked {
            command.env(crate::env::ONESHOT_RUN, checkout.parent().unwrap());
        }
        let spawned = command.output().unwrap();
        String::from_utf8_lossy(&spawned.stdout)
            .trim()
            .parse::<i32>()
            .unwrap()
    };
    let identity = |pid: i32| {
        crate::process::identity::capture(pid as u32)
            .unwrap()
            .unwrap()
    };
    let survivor = identity(detached(true, &checkout));
    let moved = identity(detached(true, &fixture.workspace.path));
    let bystander = identity(detached(false, &checkout));
    fixture.reviews.recover(&*fixture.daemon).await.unwrap();
    // Alive as recovery judges it: a stopped orphan may wait to be reaped.
    let alive = |process| crate::process::identity::alive(process).unwrap();
    assert!(!alive(&survivor), "the survivor runs on");
    assert!(!alive(&moved), "the survivor elsewhere runs on");
    assert!(alive(&bystander), "the bystander was stopped");
    crate::process::identity::stop_verified(&[bystander])
        .await
        .unwrap();
    assert!(!checkout.parent().unwrap().exists());
    let review = fixture.shown(queued).await.unwrap();
    assert_eq!((review.state, review.head), (ReviewState::Queued, None));
}

#[tokio::test]
async fn a_review_checkout_holds_its_repository_until_it_is_removed() {
    let fixture = fixture(&[], "").await;
    let (reviews, daemon) = (&fixture.reviews, &*fixture.daemon);
    let repository = fixture.workspace.repository().clone();
    let id = fixture.workspace.id.clone();
    let queued = (reviews.store)
        .run(move |db| queue_absent(db, &id))
        .await
        .unwrap();
    assert!(reviews.ensure_none_in(daemon, &repository).await.is_err());
    // Its records go with its workspace while its run still removes the
    // checkout.
    let run = fixture.run();
    let checkout = reviews.hold_checkout(queued, &repository, &run);
    (reviews.store)
        .run(move |db| Ok(db.execute("DELETE FROM reviews WHERE id=?1", [queued])?))
        .await
        .unwrap();
    assert!(reviews.ensure_none_in(daemon, &repository).await.is_err());
    // The next review there waits for it.
    let next = ReviewId(queued.0 + 1);
    assert!(matches!(
        reviews.try_hold_run(next, &repository, &run),
        Err(false)
    ));
    drop(checkout);
    reviews.ensure_none_in(daemon, &repository).await.unwrap();
    drop(reviews.try_hold_run(next, &repository, &run).unwrap());
    // A checkout its run could not remove waits for a restarted daemon,
    // which the next review there is told.
    reviews.hold_checkout(queued, &repository, &run).keep();
    assert!(reviews.ensure_none_in(daemon, &repository).await.is_err());
    assert!(matches!(
        reviews.try_hold_run(next, &repository, &run),
        Err(true)
    ));
}

#[tokio::test]
async fn a_run_left_in_the_repository_directory_holds_back_its_repository() {
    let fixture = fixture(&[], "").await;
    let (reviews, daemon) = (&fixture.reviews, &*fixture.daemon);
    let repository = fixture.workspace.repository().clone();
    fixture.runs.create().unwrap();
    reviews.ensure_none_in(daemon, &repository).await.unwrap();
    let run = fixture.left_run(ReviewId(7));
    std::fs::create_dir_all(run.join("checkout")).unwrap();
    assert!(reviews.ensure_none_in(daemon, &repository).await.is_err());
    assert!(run.exists());
    std::fs::remove_dir_all(&run).unwrap();
    reviews.ensure_none_in(daemon, &repository).await.unwrap();
}

#[tokio::test]
async fn the_next_review_receives_the_history_and_checks_fixes() {
    let script = "printf '%s' \"$1\" > \"$(dirname '{review}')/prompt\"\ncat '{review}'";
    let fixture = fixture(&[("fake", script)], "agent = 'fake'").await;
    let first = fixture.review().await;
    let thread = &first.threads[0];
    fixture.resolve(thread, Some(Outcome::Fixed)).await.unwrap();
    fixture.review().await;
    let prompt = written(&fixture, "prompt");
    assert!(
        prompt.contains("### Finding 1.1 should-fix at tracked:1: Unclear"),
        "{prompt}"
    );
    assert!(prompt.contains("Outcome: fixed; check it"), "{prompt}");
}

#[tokio::test]
async fn the_reviewer_answers_a_post_in_its_finding_s_thread() {
    // Asked to answer, it writes its prompt and answers; else it reviews.
    let script = "case \"$1\" in *'reply in `answer`'*)\n\
        printf '%s' \"$1\" > \"$(dirname '{review}')/prompt\"\n\
        echo '{\"answer\":\"It is reachable from parse.\"}';;\n\
        *) cat '{review}';; esac";
    let fixture = fixture(&[("fake", script)], "agent = 'fake'").await;
    let thread = fixture.review().await.threads.remove(0);
    let mut answered = fixture.daemon.answered.subscribe();
    fixture
        .records
        .post_as_holder(thread.number, "Is this reachable?");
    fixture.reviews.answer_posts();
    let answered = answered.wait_for(|answered| !answered.is_empty()).await;
    assert_eq!(
        answered.unwrap().as_slice(),
        [(thread.id, "It is reachable from parse.".to_owned())]
    );
    let prompt = written(&fixture, "prompt");
    assert!(prompt.contains("Author: Is this reachable?"), "{prompt}");
    assert!(prompt.contains("### Finding 1.1 should-fix"), "{prompt}");
}

#[tokio::test]
async fn removing_the_workspace_stops_an_answer_being_made() {
    // Asked to answer, it runs until it is stopped; else it reviews.
    let script = format!(
        "case \"$1\" in *'reply in `answer`'*) {};;\n*) cat '{{review}}';; esac",
        until_tests_end()
    );
    let fixture = fixture(&[("fake", &script)], "agent = 'fake'").await;
    let thread = fixture.review().await.threads.remove(0);
    fixture.records.post_as_holder(thread.number, "Really?");
    fixture.reviews.answer_posts();
    // Its checkout exists only once the answer is under way.
    while !fixture.run().join("checkout").exists() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let mut ended = fixture.reviews.ends();
    let (reviews, daemon) = (&fixture.reviews, &*fixture.daemon);
    reviews
        .stop_in_workspace(daemon, &fixture.workspace.id)
        .await;
    ended.changed().await.unwrap();
    assert!(fixture.daemon.answered.borrow().is_empty());
    assert!(!fixture.run().exists());
}

#[tokio::test]
async fn an_answer_that_cannot_be_posted_is_not_asked_for_again() {
    let script = "case \"$1\" in *'reply in `answer`'*) echo '{\"answer\":\"Yes.\"}';;\n\
        *) cat '{review}';; esac";
    let fixture = fixture(&[("fake", script)], "agent = 'fake'").await;
    let thread = fixture.review().await.threads.remove(0);
    fixture
        .daemon
        .answer_fails
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut answered = fixture.daemon.answered.subscribe();
    fixture.records.post_as_holder(thread.number, "Really?");
    fixture.reviews.answer_posts();
    answered
        .wait_for(|answered| !answered.is_empty())
        .await
        .unwrap();
    // The post still waits, but the reviewer ran for it.
    let awaited = (fixture.reviews.store)
        .run({
            let records = fixture.records.clone();
            move |db| crate::daemon::Records::awaiting_answers(&*records, db)
        })
        .await
        .unwrap();
    assert_eq!(awaited.len(), 1);
    assert!(fixture.reviews.unanswered(&awaited).is_empty());
    // The holder's next post waits for an answer again.
    fixture.records.post_as_holder(thread.number, "Still?");
    let records = fixture.records.clone();
    let awaited = (fixture.reviews.store)
        .run(move |db| crate::daemon::Records::awaiting_answers(&*records, db))
        .await
        .unwrap();
    assert_eq!(fixture.reviews.unanswered(&awaited), awaited);
}

#[tokio::test]
async fn later_reviews_receive_a_false_positive_until_it_is_reopened() {
    let script = "printf '%s' \"$1\" > \"$(dirname '{review}')/prompt\"\ncat '{review}'";
    let fixture = fixture(&[("fake", script)], "agent = 'fake'").await;
    let thread = fixture.review().await.threads.remove(0);
    let reason = "The field's type names it.".to_owned();
    let outcome = Some(Outcome::FalsePositive { reason });
    fixture.resolve(&thread, outcome).await.unwrap();
    fixture.review().await;
    let listed = written(&fixture, "prompt");
    assert!(listed.contains("The field's type names it."), "{listed}");
    fixture.resolve(&thread, None).await.unwrap();
    fixture.review().await;
    let reopened = written(&fixture, "prompt");
    assert!(
        !reopened.contains("The field's type names it."),
        "{reopened}"
    );
}

#[tokio::test]
async fn a_finding_keeps_its_outcome_when_its_false_positive_cannot_be_kept() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let review = fixture.review().await;
    // A directory where the file belongs can be neither read nor replaced.
    std::fs::create_dir_all(fixture.false_positives()).unwrap();
    let reason = "The field's type names it.".to_owned();
    let outcome = Some(Outcome::FalsePositive { reason });
    assert!(fixture.resolve(&review.threads[0], outcome).await.is_err());
    let review = fixture.shown(review.id).await.unwrap();
    assert!(review.threads[0].is_open());
}

#[tokio::test]
async fn the_false_positives_go_back_when_the_outcome_cannot_be_stored() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let thread = fixture.review().await.threads.remove(0);
    let reason = "The field's type names it.".to_owned();
    let outcome = Some(Outcome::FalsePositive { reason });
    fixture.resolve(&thread, outcome).await.unwrap();
    let path = fixture.false_positives();
    let kept = std::fs::read_to_string(&path).unwrap();
    let reopening = fixture
        .reviews
        .keep_false_positive(&*fixture.daemon, &thread, None);
    let reopening = reopening.await.unwrap().unwrap();
    assert_ne!(std::fs::read_to_string(&path).unwrap(), kept);
    reopening.undo().await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), kept);
}

#[tokio::test]
async fn an_invalid_false_positives_file_fails_reviews_of_the_repository() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let path = fixture.false_positives();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "[[false_positive]]\nclaim = \"Unchecked\"\n").unwrap();
    let review = fixture.review().await;
    assert_eq!(review.state, ReviewState::Failed);
    assert_eq!(review.failure, Some(FailureReason::InvalidFalsePositives));
}

#[tokio::test]
async fn the_workspace_hears_its_reviews_except_its_own_cancels() {
    let slow = until_tests_end();
    let fixture = fixture(
        &[("fake", "cat '{review}'"), ("slow", &slow)],
        "agent = 'fake'",
    )
    .await;
    let (reviews, daemon) = (&fixture.reviews, &*fixture.daemon);
    fixture.review().await;
    assert_eq!(fixture.messages().len(), 1, "{:?}", fixture.messages());
    // Its coordinator hears one line of each.
    let notices = daemon.notices.lock().unwrap().clone();
    let [notice] = notices.as_slice() else {
        panic!("{notices:?}");
    };
    assert!(notice.starts_with("review "), "{notice}");

    // A reviewer that runs until it is cancelled.
    let queued = fixture.queue(Some("slow".parse().unwrap())).await.unwrap();
    let own = Some(fixture.workspace.id.clone());
    (reviews.cancel(daemon, None, Some(queued.id), own))
        .await
        .unwrap();
    assert_eq!(fixture.messages().len(), 1);
    let queued = fixture.queue(Some("slow".parse().unwrap())).await.unwrap();
    (reviews.cancel(daemon, None, Some(queued.id), None))
        .await
        .unwrap();
    assert_eq!(fixture.messages().len(), 2, "{:?}", fixture.messages());
}

#[tokio::test]
async fn a_gate_waits_for_a_review_of_head_with_every_finding_resolved() {
    let fixture = fixture(
        &[("fake", "cat '{review}'")],
        "agent = 'fake'\nrequire_for_ready = 'agents'",
    )
    .await;
    let (reviews, daemon) = (&fixture.reviews, &*fixture.daemon);
    let workspace = &fixture.workspace;
    let caller = shoal_daemon_base::callers::Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let agent = Some(&caller);
    let head = crate::daemon::Spaces::head(daemon, workspace)
        .await
        .unwrap();
    let require = |gate, caller| reviews.require(daemon, workspace, gate, &head, caller);
    require(Gate::Pr, agent).await.unwrap();
    // "agents" leaves the user's own commands alone.
    require(Gate::Ready, None).await.unwrap();

    let mut ended = reviews.ends();
    assert!(require(Gate::Ready, agent).await.is_err());
    ended.changed().await.unwrap();

    let review = (reviews.latest(daemon, &fixture.selector())).await.unwrap();
    let refused = require(Gate::Ready, agent).await.unwrap_err().to_string();
    assert!(refused.contains("1.1 should-fix"), "{refused}");
    // Unresolved findings at HEAD need answers, not another review.
    let latest = (reviews.latest(daemon, &fixture.selector())).await.unwrap();
    assert_eq!(latest.id, review.id);

    let outcome = Outcome::WontFix {
        reason: "The name matches the file".into(),
    };
    fixture
        .resolve(&review.threads[0], Some(outcome))
        .await
        .unwrap();
    require(Gate::Ready, agent).await.unwrap();
}

#[tokio::test]
async fn each_review_of_a_workspace_runs_in_one_checkout_in_its_repository_directory() {
    let script = "pwd >> \"$(dirname '{review}')/cwd\"\ncat '{review}'";
    let fixture = fixture(&[("fake", script)], "agent = 'fake'").await;
    for _ in 0..2 {
        let review = fixture.review().await;
        assert_eq!(review.state, ReviewState::Finished, "{review:?}");
    }
    let cwd = written(&fixture, "cwd");
    let cwd: Vec<_> = cwd.lines().map(std::path::Path::new).collect();
    assert_eq!(cwd[0], cwd[1]);
    // A temporary directory's path may lead through a link.
    let checkout = std::path::Path::new(fixture.workspace.id.as_str()).join("checkout");
    assert!(cwd[0].ends_with(&checkout), "{cwd:?}");
}

/// A reviewer that records each run as `new` or `resume <session>` in `runs`,
/// its directory in `cwd` and the latest resumed run's prompt in `resumed`,
/// reports session
/// `s<run>`, or the one it resumes, as Codex does, and cannot resume while
/// `stale` exists.
const RESUMER: &str = r#"dir="$(dirname '{review}')"
pwd >> "$dir/cwd"
if [ "$1" = resume ]; then echo "resume $2" >> "$dir/runs"; session=$2; printf '%s' "$3" > "$dir/resumed"
else echo new >> "$dir/runs"; session="s$(wc -l < "$dir/runs" | tr -d ' ')"; fi
echo "session id: $session" >&2
[ "$1" = resume ] && [ -e "$dir/stale" ] && exit 1
cat '{review}'"#;

/// What makes [`RESUMER`] resume its sessions as Codex does.
const RESUMES: &str =
    "base = 'codex'\nreview_resume = ['resume', '{session}', '{prompt}', '{schema}']\n";

#[tokio::test]
async fn later_reviews_resume_the_session_of_the_agent_that_reviewed_last() {
    let reviewers = [("fake", RESUMER), ("other", RESUMER)];
    let fixture = fixture_with(&reviewers, "agent = 'fake'", RESUMES).await;
    let fixture = &fixture;
    let review = |agent| async move {
        let review = fixture.review_by(agent).await;
        assert_eq!(review.state, ReviewState::Finished, "{review:?}");
    };
    review(None).await;
    review(None).await;
    // The resumed session is given the instructions with the history again.
    let resumed = written(fixture, "resumed");
    assert!(resumed.contains("# Earlier reviews"), "{resumed}");
    // Another agent's review starts its own, after which the configured
    // agent starts a new one too.
    review(Some("other")).await;
    review(None).await;
    // A session that cannot be resumed gives way to a new one, which the
    // next review resumes.
    std::fs::write(fixture.written("stale"), "").unwrap();
    review(None).await;
    std::fs::remove_file(fixture.written("stale")).unwrap();
    review(None).await;
    let runs = written(fixture, "runs");
    assert_eq!(
        runs.lines().collect::<Vec<_>>(),
        [
            "new",
            "resume s1",
            "new",
            "new",
            "resume s4",
            "new",
            "resume s6"
        ]
    );
}

#[tokio::test]
async fn a_review_runs_beside_a_run_that_could_not_be_removed_in_a_new_session() {
    let fixture = fixture_with(&[("fake", RESUMER)], "agent = 'fake'", RESUMES).await;
    let review = || async {
        let review = fixture.review().await;
        assert_eq!(review.state, ReviewState::Finished, "{review:?}");
        review
    };
    review().await;
    // A run that could not be removed keeps the workspace's place until a
    // restart.
    let kept = ReviewId(999);
    let run = fixture.left_run(kept);
    let repository = fixture.workspace.repository().clone();
    (fixture.reviews)
        .hold_checkout(kept, &repository, &run)
        .keep();
    let beside = review().await;
    assert!(run.exists());
    // Once it is gone, the next review continues the session there.
    fixture.reviews.checkouts.lock().unwrap().remove(&kept);
    std::fs::remove_dir_all(&run).unwrap();
    review().await;
    let runs = written(&fixture, "runs");
    assert_eq!(
        runs.lines().collect::<Vec<_>>(),
        ["new", "new", "resume s1"]
    );
    let cwd = written(&fixture, "cwd");
    let cwd: Vec<_> = cwd.lines().map(std::path::Path::new).collect();
    let workspace = fixture.workspace.id.as_str();
    let elsewhere = std::path::Path::new(&format!("{workspace}-{}", beside.id)).join("checkout");
    assert!(cwd[1].ends_with(&elsewhere), "{cwd:?}");
    assert_eq!(cwd[0], cwd[2]);
}

#[tokio::test]
async fn a_review_queued_while_a_cancelled_one_ends_waits_for_its_run() {
    let fixture = fixture(&[("slow", &until_tests_end())], "agent = 'slow'").await;
    let (reviews, daemon) = (&fixture.reviews, &*fixture.daemon);
    let first = fixture.queue(None).await.unwrap();
    fixture.until(first.id, ReviewState::Running).await;
    (reviews.cancel(daemon, None, Some(first.id), None))
        .await
        .unwrap();
    let second = fixture.queue(None).await.unwrap();
    let mut changed = reviews.store.watch_changes();
    while fixture.shown(second.id).await.unwrap().state == ReviewState::Queued {
        changed.changed().await.unwrap();
    }
    let state = fixture.shown(second.id).await.unwrap().state;
    assert_eq!(state, ReviewState::Running);
    // It holds the workspace's place, which it takes before it starts, not
    // one beside the cancelled run.
    let held = reviews.checkouts.lock().unwrap()[&second.id].clone();
    let place = HeldCheckout::new(fixture.workspace.repository(), &fixture.run());
    assert_eq!(held, place);
}

#[tokio::test]
async fn a_run_that_cannot_be_removed_fails_its_review_instead_of_starting_over() {
    use std::os::unix::fs::PermissionsExt;
    // Root deletes past permissions, as in CI's containers.
    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let fixture = fixture(&[], "").await;
    let (reviews, daemon) = (&fixture.reviews, &*fixture.daemon);
    let repository = fixture.workspace.repository().clone();
    let stuck = fixture.started().await;
    let run = fixture.left_run(stuck);
    std::fs::create_dir_all(run.join("checkout").join("file")).unwrap();
    // Without write access to the run, its checkout cannot go.
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o500)).unwrap();
    reviews.recover(daemon).await.unwrap();
    assert_eq!(fixture.messages().len(), 1);
    // A later start that still cannot remove it delivers the review once.
    reviews.recover(daemon).await.unwrap();
    assert_eq!(fixture.messages().len(), 1);
    // Nothing runs there, so only the checkout is held, not a slot.
    assert!(daemon.held.lock().unwrap().is_empty());
    assert!(reviews.ensure_none_in(daemon, &repository).await.is_err());
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    let review = fixture.shown(stuck).await.unwrap();
    assert_eq!(review.state, ReviewState::Failed, "{review:?}");
    assert!(run.join("checkout").exists());
    // The next start removes it.
    reviews.recover(daemon).await.unwrap();
    assert!(!run.exists());
}

#[tokio::test]
async fn a_run_recovery_cannot_stop_keeps_its_checkout_and_admission() {
    let fixture = fixture(&[], "").await;
    let (reviews, daemon) = (&fixture.reviews, &*fixture.daemon);
    let repository = fixture.workspace.repository().clone();
    let stuck = fixture.started().await;
    // The second run's review went with its workspace.
    let gone = ReviewId(stuck.0 + 1);
    let runs = [
        fixture.left_run(stuck),
        fixture.runs.root().join(gone.to_string()),
    ];
    std::fs::create_dir_all(&runs[1]).unwrap();
    std::fs::write(runs[1].join("review"), gone.to_string()).unwrap();
    for run in &runs {
        std::fs::create_dir_all(run.join("checkout")).unwrap();
    }
    // What the stopped daemon's reviewers left cannot be proven stopped.
    (reviews.runs.fail_inventory).store(true, std::sync::atomic::Ordering::SeqCst);
    reviews.recover(daemon).await.unwrap();
    let review = fixture.shown(stuck).await.unwrap();
    assert_eq!(review.state, ReviewState::Failed, "{review:?}");
    assert!(runs.iter().all(|run| run.join("checkout").exists()));
    // Until a restart, the repository stays held and the slots used.
    let held = reviews.checkouts.lock().unwrap().clone();
    let both = [stuck, gone].into_iter().zip(&runs).map(|(id, run)| {
        let mut held = HeldCheckout::new(&repository, run);
        held.kept = true;
        (id, held)
    });
    assert_eq!(held, both.collect());
    let slots = daemon.held.lock().unwrap().clone();
    assert_eq!(slots, [repository.clone(), repository]);
}

/// The test process a survivor outlives no longer than.
const SURVIVOR_OWNER: &str = "REVIEW_TEST_SURVIVOR_OWNER";

/// Sleeps as a process a stopped daemon's reviewer left, for the recovery
/// test, which starts it: until that test's process exits, for at most ten
/// minutes.
#[test]
#[ignore]
fn sleep_as_a_survivor() {
    let Some(owner) = std::env::var(SURVIVOR_OWNER)
        .ok()
        .and_then(|owner| owner.parse::<i32>().ok())
    else {
        return;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    // SAFETY: signal 0 only checks that the process exists.
    while std::time::Instant::now() < deadline && unsafe { libc::kill(owner, 0) } == 0 {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
