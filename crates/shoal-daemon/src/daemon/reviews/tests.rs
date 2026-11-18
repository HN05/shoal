use crate::shoal_dirs::{Contents, ShoalDir};
use crate::time::UnixTime;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::BranchName;
use std::sync::Arc;

use super::*;
use crate::model::Workspace;
use crate::{
    daemon::test_support::{configure, install_test_script, manager, repository, until_tests_end},
    reviews::{FailureReason, ReviewState, Severity},
};
use shoal_protocol::space_names::SpaceSelector;

const REVIEW: &str = r#"{"summary":"One problem.","verdict":"comment","checked":[],
"findings":[{"path":"tracked","line":1,"severity":"should-fix","title":"Unclear","body":"Name it."}]}"#;

struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    workspace: Workspace,
    /// The repository's directory of review runs.
    runs: ShoalDir,
}

/// A workspace whose reviewers are `[agents.<name>]` scripts, in which
/// `{review}` names a file holding [`REVIEW`], with the review scheduler and
/// the agent queue, under a fixed agent limit, running when there are any.
async fn fixture(reviewers: &[(&str, &str)], review: &str) -> Fixture {
    fixture_with(reviewers, review, "").await
}

/// [`fixture`] with `settings` in each reviewer's table.
async fn fixture_with(reviewers: &[(&str, &str)], review: &str, settings: &str) -> Fixture {
    let (root, manager) = manager().await;
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
    config.push_str("[machine]\nmax_heavy_agents = 4\n[machine.overload.disk]\nenabled = false\n");
    configure(&manager, &config).await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    if !reviewers.is_empty() {
        tokio::spawn(run(manager.clone()));
        tokio::spawn(crate::daemon::queued_starts::run(manager.clone()));
    }
    Fixture {
        _root: root,
        manager,
        workspace,
        runs: ShoalDir::of(&repo, Contents::ReviewRuns),
    }
}

impl Fixture {
    /// The directory of the runs of the workspace's reviews.
    fn run(&self) -> std::path::PathBuf {
        self.runs.root().join(self.workspace.id.as_str())
    }

    /// The workspace's run, as review `id` left it, without a checkout.
    fn left_run(&self, id: ReviewId) -> std::path::PathBuf {
        self.runs.create().unwrap();
        let run = self.run();
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("review"), id.to_string()).unwrap();
        run
    }

    /// A review of the workspace, once its run ended and delivered it.
    async fn review(&self) -> Review {
        self.review_of(&self.workspace.id).await
    }

    async fn review_of(&self, workspace: &SpaceId) -> Review {
        self.review_by(workspace, None).await
    }

    /// [`Self::review_of`] by `agent`, else the configured reviewer.
    async fn review_by(&self, workspace: &SpaceId, agent: Option<&str>) -> Review {
        let mut ended = self.manager.reviews.ended.subscribe();
        let queued = self
            .manager
            .queue_review(
                &SpaceSelector::id(workspace),
                ReviewTarget::Workspace,
                agent.map(crate::agent::Agent::from),
                Default::default(),
            )
            .await
            .unwrap();
        ended.changed().await.unwrap();
        self.manager.await_review(queued.id).await.unwrap()
    }
}

#[tokio::test]
async fn a_reviewer_runs_with_the_requested_or_configured_model_and_effort() {
    let script = "printf '%s\\n' \"$@\" > \"$(dirname '{review}')/args\"\ncat '{review}'";
    let profile = "agent = 'modeled'\n[agents.modeled]\nbase = 'fake'\neffort = 'low'\n\
        model_args = ['--model', '{model}']\neffort_args = ['--effort', '{effort}']\n";
    let fixture = fixture(&[("fake", script)], profile).await;
    let mut ended = fixture.manager.reviews.ended.subscribe();
    let model = crate::agent::ModelChoice {
        model: Some("m1".into()),
        effort: None,
    };
    let queued = fixture
        .manager
        .queue_review(
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            None,
            model.clone(),
        )
        .await
        .unwrap();
    assert_eq!(queued.model, model);
    ended.changed().await.unwrap();
    let review = fixture.manager.await_review(queued.id).await.unwrap();
    assert_eq!(review.state, ReviewState::Finished, "{review:?}");
    let args = std::fs::read_to_string(fixture._root.path().join("args")).unwrap();
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
    assert_eq!(review.findings[0].severity, Severity::ShouldFix);
    assert_eq!(
        review.findings[0].id.to_string(),
        format!("{}.1", review.id)
    );
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

    configure(&fixture.manager, "[review]\nagent = 'first'\nfallback_agent = 'second'\n[agents.first]\ncommand = ['false']\nreview = ['{prompt}', '{schema}']\n[agents.second]\ncommand = ['echo']\nreview = ['{prompt}', '{schema}']\n").await;
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
async fn a_review_protection_stops_is_queued_again() {
    let fixture = fixture(&[("slow", &until_tests_end())], "agent = 'slow'").await;
    let manager = &fixture.manager;
    let queued = manager
        .queue_review(
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let mut changed = manager.store.watch_changes();
    while manager.review(queued.id).await.unwrap().state != ReviewState::Running {
        changed.changed().await.unwrap();
    }
    assert!(
        manager
            .stop_agent_for_overload("critical memory pressure")
            .await
    );
    while manager.review(queued.id).await.unwrap().state != ReviewState::Queued {
        changed.changed().await.unwrap();
    }
    let review = manager.review(queued.id).await.unwrap();
    assert_eq!((review.head, review.started_at), (None, None));
    // A restarted daemon keeps it waiting for healthy readings.
    let id = queued.id;
    assert!(
        manager
            .store
            .run(move |db| store::waits_for_recovery(db, id))
            .await
            .unwrap()
    );
    // It waits for healthy readings; a cancel still ends it.
    let mut ended = manager.reviews.ended.subscribe();
    let cancelled = manager
        .cancel_review(None, Some(queued.id), None)
        .await
        .unwrap();
    assert_eq!(cancelled.state, ReviewState::Cancelled);
    ended.changed().await.unwrap();
}

#[tokio::test]
async fn cancelling_stops_the_reviewer_and_removes_its_checkout() {
    let fixture = fixture(&[("slow", &until_tests_end())], "agent = 'slow'").await;
    let manager = &fixture.manager;
    let queued = manager
        .queue_review(
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let mut changed = manager.store.watch_changes();
    while manager.review(queued.id).await.unwrap().state != ReviewState::Running {
        changed.changed().await.unwrap();
    }
    let mut ended = manager.reviews.ended.subscribe();
    let cancelled = manager
        .cancel_review(
            Some(&SpaceSelector::id(&fixture.workspace.id)),
            None,
            Some(fixture.workspace.id.clone()),
        )
        .await
        .unwrap();
    assert_eq!(cancelled.state, ReviewState::Cancelled);
    ended.changed().await.unwrap();
    assert!(!fixture.run().exists());
    let again = manager.cancel_review(None, Some(queued.id), None).await;
    assert!(again.is_err());
}

#[tokio::test]
async fn a_reviewer_needs_a_review_template() {
    let fixture = fixture(&[], "").await;
    let refused = fixture
        .manager
        .queue_review(
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            Some("pi".parse().unwrap()),
            Default::default(),
        )
        .await;
    assert!(refused.is_err());
    let unset = fixture
        .manager
        .queue_review(
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            None,
            Default::default(),
        )
        .await;
    assert!(unset.is_err());
}

#[tokio::test]
async fn startup_removes_interrupted_checkouts_and_queues_their_reviews_again() {
    let fixture = fixture(&[], "").await;
    let manager = &fixture.manager;
    let id = fixture.workspace.id.clone();
    let queued = manager
        .store
        .run(move |db| {
            store::queue(
                db,
                &id,
                &ReviewTarget::Workspace,
                &"absent".into(),
                &Default::default(),
                UnixTime::from_seconds(1),
            )
        })
        .await
        .unwrap()
        .id();
    manager
        .store
        .run(move |db| {
            store::start(
                db,
                queued,
                &"head".into(),
                &"base".into(),
                UnixTime::from_seconds(2),
            )
        })
        .await
        .unwrap();
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
                "\"$0\" --exact daemon::reviews::tests::sleep_as_a_survivor --ignored >/dev/null 2>&1 & echo $!",
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
    manager.recover_reviews().await.unwrap();
    // Alive as recovery judges it: a stopped orphan may wait to be reaped.
    let alive = |process| crate::process::identity::alive(process).unwrap();
    assert!(!alive(&survivor), "the survivor runs on");
    assert!(!alive(&moved), "the survivor elsewhere runs on");
    assert!(alive(&bystander), "the bystander was stopped");
    crate::process::identity::stop_verified(&[bystander])
        .await
        .unwrap();
    assert!(!checkout.parent().unwrap().exists());
    let review = manager.review(queued).await.unwrap();
    assert_eq!((review.state, review.head), (ReviewState::Queued, None));
}

#[tokio::test]
async fn a_review_checkout_holds_its_repository_until_it_is_removed() {
    let fixture = fixture(&[], "").await;
    let manager = &fixture.manager;
    let repository = fixture.workspace.repository().clone();
    let id = fixture.workspace.id.clone();
    let queued = manager
        .store
        .run(move |db| {
            store::queue(
                db,
                &id,
                &ReviewTarget::Workspace,
                &"absent".into(),
                &Default::default(),
                UnixTime::from_seconds(1),
            )
        })
        .await
        .unwrap()
        .id();
    assert!(manager.ensure_no_reviews_in(&repository).await.is_err());
    // Its records go with its workspace while its run still removes the
    // checkout.
    let run = fixture.run();
    let checkout = manager.reviews.hold_checkout(queued, &repository, &run);
    manager
        .store
        .run(move |db| Ok(db.execute("DELETE FROM reviews WHERE id=?1", [queued])?))
        .await
        .unwrap();
    assert!(manager.ensure_no_reviews_in(&repository).await.is_err());
    // The next review there waits for it.
    let next = ReviewId(queued.0 + 1);
    assert!(matches!(
        manager.reviews.hold_run(next, &repository, &run),
        Err(false)
    ));
    drop(checkout);
    manager.ensure_no_reviews_in(&repository).await.unwrap();
    drop(manager.reviews.hold_run(next, &repository, &run).unwrap());
    // A checkout its run could not remove waits for a restarted daemon,
    // which the next review there is told.
    (manager.reviews)
        .hold_checkout(queued, &repository, &run)
        .keep();
    assert!(manager.ensure_no_reviews_in(&repository).await.is_err());
    assert!(matches!(
        manager.reviews.hold_run(next, &repository, &run),
        Err(true)
    ));
}

#[tokio::test]
async fn a_replaced_checkout_is_not_reviewed() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let repository = fixture
        .manager
        .repository(fixture.workspace.repository())
        .await
        .unwrap();
    // Another worktree on the same branch at the workspace's path.
    let path = fixture.workspace.path.to_str().unwrap();
    crate::daemon::test_support::git(&repository.path, &["worktree", "remove", "--force", path]);
    crate::daemon::test_support::git(
        &repository.path,
        &[
            "worktree",
            "add",
            "-q",
            path,
            fixture.workspace.branch().map(BranchName::as_str).unwrap(),
        ],
    );
    let review = fixture.review().await;
    assert_eq!(review.state, ReviewState::Failed, "{review:?}");
    assert_eq!(review.failure, Some(FailureReason::ExecutionFailed));
    assert_eq!(review.reviewer, None);
}

#[tokio::test]
async fn removing_the_workspace_stops_its_review() {
    let fixture = fixture(&[("slow", &until_tests_end())], "agent = 'slow'").await;
    let manager = &fixture.manager;
    let queued = manager
        .queue_review(
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let mut changed = manager.store.watch_changes();
    while manager.review(queued.id).await.unwrap().state != ReviewState::Running {
        changed.changed().await.unwrap();
    }
    let mut ended = manager.reviews.ended.subscribe();
    manager
        .remove_selected(
            &SpaceSelector::id(&fixture.workspace.id),
            crate::removal::UnsavedWork::All,
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    ended.changed().await.unwrap();
    assert!(manager.review(queued.id).await.is_err());
    assert!(!fixture.run().exists());
}

#[tokio::test]
async fn the_next_review_receives_the_history_and_checks_fixes() {
    let script = "printf '%s' \"$1\" > \"$(dirname '{review}')/prompt\"\ncat '{review}'";
    let fixture = fixture(&[("fake", script)], "agent = 'fake'").await;
    let first = fixture.review().await;
    let finding = first.findings[0].id;
    fixture
        .manager
        .resolve_finding(finding, Some(Outcome::Fixed))
        .await
        .unwrap();
    fixture.review().await;
    let prompt_file = fixture._root.path().join("prompt");
    let prompt = std::fs::read_to_string(prompt_file).unwrap();
    assert!(
        prompt.contains(&format!(
            "### Finding {finding} (should-fix) at tracked:1: Unclear"
        )),
        "{prompt}"
    );
}

#[tokio::test]
async fn later_reviews_receive_a_false_positive_until_it_is_reopened() {
    let script = "printf '%s' \"$1\" > \"$(dirname '{review}')/prompt\"\ncat '{review}'";
    let fixture = fixture(&[("fake", script)], "agent = 'fake'").await;
    let prompt = || std::fs::read_to_string(fixture._root.path().join("prompt")).unwrap();
    let finding = fixture.review().await.findings[0].id;
    let reason = "The field's type names it.".to_owned();
    fixture
        .manager
        .resolve_finding(finding, Some(Outcome::FalsePositive { reason }))
        .await
        .unwrap();
    fixture.review().await;
    let listed = prompt();
    assert!(listed.contains("The field's type names it."), "{listed}");
    fixture
        .manager
        .resolve_finding(finding, None)
        .await
        .unwrap();
    fixture.review().await;
    let reopened = prompt();
    assert!(
        !reopened.contains("The field's type names it."),
        "{reopened}"
    );
}

#[tokio::test]
async fn a_finding_keeps_its_outcome_when_its_false_positive_cannot_be_kept() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let finding = fixture.review().await.findings[0].id;
    let repository = fixture.workspace.repository();
    let path = fixture
        .manager
        .false_positives_path(repository)
        .await
        .unwrap()
        .unwrap();
    // A directory where the file belongs can be neither read nor replaced.
    std::fs::create_dir_all(&path).unwrap();
    let reason = "The field's type names it.".to_owned();
    let resolved = fixture
        .manager
        .resolve_finding(finding, Some(Outcome::FalsePositive { reason }))
        .await;
    assert!(resolved.is_err());
    let review = fixture.manager.review(finding.review).await.unwrap();
    assert_eq!(review.findings[0].outcome, None);
}

#[tokio::test]
async fn the_false_positives_go_back_when_the_outcome_cannot_be_stored() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let finding = fixture.review().await.findings[0].id;
    let reason = "The field's type names it.".to_owned();
    fixture
        .manager
        .resolve_finding(finding, Some(Outcome::FalsePositive { reason }))
        .await
        .unwrap();
    let repository = fixture.workspace.repository();
    let path = fixture
        .manager
        .false_positives_path(repository)
        .await
        .unwrap()
        .unwrap();
    let kept = std::fs::read_to_string(&path).unwrap();
    let missing = FindingId {
        review: ReviewId(999),
        number: 1,
    };
    fixture
        .manager
        .resolve_finding(finding, Some(Outcome::Duplicate { of: missing }))
        .await
        .unwrap_err();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), kept);
}

#[tokio::test]
async fn an_invalid_false_positives_file_fails_reviews_of_the_repository() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let repository = fixture.workspace.repository();
    let path = fixture
        .manager
        .false_positives_path(repository)
        .await
        .unwrap()
        .unwrap();
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
    let manager = &fixture.manager;
    fixture.review().await;
    let messages = manager
        .agent_messages(&SpaceSelector::id(&fixture.workspace.id))
        .await
        .unwrap();
    assert_eq!(messages.len(), 1, "{messages:?}");

    // A reviewer that runs until it is cancelled.
    let queued = manager
        .queue_review(
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            Some("slow".parse().unwrap()),
            Default::default(),
        )
        .await
        .unwrap();
    let before = manager
        .agent_messages(&SpaceSelector::id(&fixture.workspace.id))
        .await
        .unwrap()
        .len();
    manager
        .cancel_review(None, Some(queued.id), Some(fixture.workspace.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        manager
            .agent_messages(&SpaceSelector::id(&fixture.workspace.id))
            .await
            .unwrap()
            .len(),
        before
    );
    let queued = manager
        .queue_review(
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            Some("slow".parse().unwrap()),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .cancel_review(None, Some(queued.id), None)
        .await
        .unwrap();
    let messages = manager
        .agent_messages(&SpaceSelector::id(&fixture.workspace.id))
        .await
        .unwrap();
    assert_eq!(messages.len(), before + 1, "{messages:?}");
}

#[tokio::test]
async fn the_coordinating_agent_hears_one_line() {
    use crate::state::AgentState;
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let manager = &fixture.manager;
    let parent = &fixture.workspace;
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    for workspace in [&parent.id, &child.id] {
        manager
            .set_agent_state(
                &SpaceSelector::from(workspace.as_str()),
                AgentState::Working,
                None,
                None,
            )
            .await
            .unwrap();
    }
    fixture.review_of(&child.id).await;
    let received: Vec<String> = manager
        .agent_messages(&SpaceSelector::id(&parent.id))
        .await
        .unwrap()
        .into_iter()
        .map(|message| message.message)
        .collect();
    let [line] = received.as_slice() else {
        panic!("{received:?}");
    };
    let prefix = format!("{} child: ", child.number);
    assert!(line.starts_with(&prefix), "{line}");
}

#[tokio::test]
async fn workspace_processes_act_on_the_reviews_of_their_tree_only() {
    use crate::{
        daemon::scope::{Caller, ScopedExecution, authorize},
        protocol::ReviewMethod,
    };
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let manager = &fixture.manager;
    let parent = &fixture.workspace;
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let parent_review = fixture.review().await;
    let child_review = fixture.review_of(&child.id).await;
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: parent.id.clone(),
                execution: Some(ScopedExecution {
                    id: "execution".into(),
                    kind: crate::daemon::workspace::ExecutionKind::Command,
                    agent_execution: None,
                }),
                granted: None,
                read_only: false,
            },
        )
        .await;
    let resolve = |review: &Review| ReviewMethod::ResolveFinding {
        workspace: None,
        finding: review.findings[0].id,
        outcome: Some(Outcome::Fixed),
    };
    // The parent's agent reaches its own and its child's reviews.
    for review in [&parent_review, &child_review] {
        authorize(manager, Some(&"scope".into()), &mut resolve(review))
            .await
            .unwrap();
    }
    manager
        .executions
        .issue_scope(
            "child-scope".into(),
            Caller {
                space_id: child.id.clone(),
                execution: None,
                granted: None,
                read_only: false,
            },
        )
        .await;
    assert!(
        authorize(
            manager,
            Some(&"child-scope".into()),
            &mut resolve(&parent_review)
        )
        .await
        .is_err()
    );
    let mut show = ReviewMethod::ShowReview {
        workspace: None,
        id: Some(parent_review.id),
    };
    assert!(
        authorize(manager, Some(&"child-scope".into()), &mut show)
            .await
            .is_err()
    );
    let mut own = ReviewMethod::ShowReview {
        workspace: None,
        id: None,
    };
    authorize(manager, Some(&"child-scope".into()), &mut own)
        .await
        .unwrap();
    assert!(
        matches!(own, ReviewMethod::ShowReview { workspace: Some(id), .. } if id == SpaceSelector::id(&child.id))
    );
}

#[tokio::test]
async fn a_gate_waits_for_a_review_of_head_with_every_finding_resolved() {
    use super::gate::Gate;
    let fixture = fixture(
        &[("fake", "cat '{review}'")],
        "agent = 'fake'\nrequire_for_ready = 'agents'",
    )
    .await;
    let manager = &fixture.manager;
    let workspace = &fixture.workspace;
    let caller = agent_caller(workspace);
    let agent = Some(&caller);
    let head = crate::daemon::links::pr::current_head(workspace)
        .await
        .unwrap();
    manager
        .require_review(workspace, Gate::Pr, &head, agent)
        .await
        .unwrap();
    // "agents" leaves the user's own commands alone.
    manager
        .require_review(workspace, Gate::Ready, &head, None)
        .await
        .unwrap();

    let mut ended = manager.reviews.ended.subscribe();
    assert!(
        manager
            .require_review(workspace, Gate::Ready, &head, agent)
            .await
            .is_err()
    );
    ended.changed().await.unwrap();

    let review = manager
        .latest_review(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let refused = manager
        .require_review(workspace, Gate::Ready, &head, agent)
        .await
        .unwrap_err();
    let refused = refused.to_string();
    assert!(
        refused.contains(&review.findings[0].id.to_string()),
        "{refused}"
    );
    // Unresolved findings at HEAD need answers, not another review.
    assert_eq!(
        manager
            .latest_review(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .id,
        review.id
    );

    manager
        .resolve_finding(
            review.findings[0].id,
            Some(Outcome::WontFix {
                reason: "The name matches the file".into(),
            }),
        )
        .await
        .unwrap();
    manager
        .require_review(workspace, Gate::Ready, &head, agent)
        .await
        .unwrap();
    // A workspace process's mark passes once the review covers HEAD.
    let marks = manager
        .mark_ready(&SpaceSelector::id(&workspace.id), Default::default(), agent)
        .await
        .unwrap()
        .marks;
    assert_eq!(marks.len(), 1);
}

#[tokio::test]
async fn all_holds_the_users_own_commands_too() {
    use super::gate::Gate;
    let fixture = fixture(&[], "require_for_pr = 'all'").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspace);
    let head = crate::daemon::links::pr::current_head(workspace)
        .await
        .unwrap();
    let refused = manager
        .require_review(workspace, Gate::Pr, &head, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains(head.short()), "{refused}");
    // require_for_ready stays off.
    let selector = SpaceSelector::id(&workspace.id);
    (manager.mark_ready(&selector, Default::default(), None))
        .await
        .unwrap();
}

/// A scoped caller of `workspace`, as its agent is.
fn agent_caller(workspace: &crate::model::Workspace) -> crate::daemon::scope::Caller {
    crate::daemon::scope::Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
    }
}

/// The frames the daemon answers `method` with, and the JSON their review
/// pieces join into.
async fn serve_pieces(manager: &Arc<Manager>, method: serde_json::Value) -> (usize, String) {
    let pages = crate::daemon::tests::serve_pages(manager, method).await;
    let pieces = pages.iter().flat_map(|page| page.as_array().unwrap());
    let json = pieces.map(|piece| piece.as_str().unwrap()).collect();
    (pages.len(), json)
}

/// Finish review after review of the workspace, each with one finding
/// whose body is `body`, returning their IDs.
async fn finished_reviews(
    manager: &Manager,
    workspace: &SpaceId,
    count: usize,
    body: &str,
) -> Vec<ReviewId> {
    let (id, body) = (workspace.to_owned(), body.to_owned());
    manager
        .store
        .write(move |tx| {
            let mut ids = Vec::new();
            for _ in 0..count {
                let review = store::queue(
                    tx,
                    &id,
                    &ReviewTarget::Workspace,
                    &"absent".into(),
                    &Default::default(),
                    UnixTime::from_seconds(1),
                )?
                .id();
                store::start(
                    tx,
                    review,
                    &"head".into(),
                    &"base".into(),
                    UnixTime::from_seconds(2),
                )?;
                let output: crate::reviews::output::ReviewerOutput = serde_json::from_value(
                    serde_json::json!({"summary": "Long.", "verdict": "comment", "checked": [],
                        "findings": [{"path": "a", "line": 1, "severity": "nit",
                            "title": "Long", "body": body}]}),
                )?;
                store::finish(
                    tx,
                    review,
                    &"absent".into(),
                    &output,
                    None,
                    UnixTime::from_seconds(3),
                )?;
                ids.push(review);
            }
            Ok(ids)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_long_listing_spans_several_frames() {
    let fixture = fixture(&[], "").await;
    let manager = fixture.manager.clone();
    finished_reviews(&manager, &fixture.workspace.id, 30, &"x".repeat(4096)).await;
    let method = serde_json::json!({"list_reviews": {"workspace": fixture.workspace.id}});
    let (frames, json) = serve_pieces(&manager, method).await;
    assert!(frames > 1, "{frames}");
    let listed: Vec<Review> = serde_json::from_str(&json).unwrap();
    assert_eq!(listed.len(), 30);
}

#[tokio::test]
async fn one_review_larger_than_a_frame_is_shown_whole() {
    let fixture = fixture(&[], "").await;
    let manager = fixture.manager.clone();
    let id = finished_reviews(&manager, &fixture.workspace.id, 1, "Long.").await[0];
    let finding = FindingId {
        review: id,
        number: 1,
    };
    for _ in 0..9 {
        manager
            .reply_to_finding(finding, "y".repeat(crate::validate::MAX_AGENT_MESSAGE_LEN))
            .await
            .unwrap();
    }
    let method = serde_json::json!({"show_review": {"id": id}});
    let (frames, json) = serve_pieces(&manager, method).await;
    assert!(frames > 1, "{frames}");
    let review: Review = serde_json::from_str(&json).unwrap();
    assert_eq!(review.findings[0].replies.len(), 9);
}

#[tokio::test]
async fn removing_the_repository_stops_its_reviews_first() {
    let fixture = fixture(&[("slow", &until_tests_end())], "agent = 'slow'").await;
    let manager = &fixture.manager;
    let queued = manager
        .queue_review(
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let mut changed = manager.store.watch_changes();
    while manager.review(queued.id).await.unwrap().state != ReviewState::Running {
        changed.changed().await.unwrap();
    }
    manager
        .remove_repository(fixture.workspace.repository())
        .await
        .unwrap();
    assert!(!fixture.runs.root().exists());
}

#[tokio::test]
async fn each_review_of_a_workspace_runs_in_one_checkout_in_its_repository_directory() {
    let script = "pwd >> \"$(dirname '{review}')/cwd\"\ncat '{review}'";
    let fixture = fixture(&[("fake", script)], "agent = 'fake'").await;
    for _ in 0..2 {
        let review = fixture.review().await;
        assert_eq!(review.state, ReviewState::Finished, "{review:?}");
    }
    let cwd = std::fs::read_to_string(fixture._root.path().join("cwd")).unwrap();
    let cwd: Vec<_> = cwd.lines().map(std::path::Path::new).collect();
    assert_eq!(cwd[0], cwd[1]);
    // A temporary directory's path may lead through a link.
    let checkout = std::path::Path::new(fixture.workspace.id.as_str()).join("checkout");
    assert!(cwd[0].ends_with(&checkout), "{cwd:?}");
    assert!(!fixture.manager.paths.state.join("reviews").exists());
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

#[tokio::test]
async fn later_reviews_resume_the_session_of_the_agent_that_reviewed_last() {
    let reviewers = [("fake", RESUMER), ("other", RESUMER)];
    let fixture = fixture_with(&reviewers, "agent = 'fake'", RESUMES).await;
    let fixture = &fixture;
    let review = |agent| async move {
        let review = fixture.review_by(&fixture.workspace.id, agent).await;
        assert_eq!(review.state, ReviewState::Finished, "{review:?}");
    };
    let root = fixture._root.path();
    review(None).await;
    review(None).await;
    // The resumed session is given the instructions with the history again.
    let resumed = std::fs::read_to_string(root.join("resumed")).unwrap();
    assert!(resumed.contains("# Earlier reviews"), "{resumed}");
    // Another agent's review starts its own, after which the configured
    // agent starts a new one too.
    review(Some("other")).await;
    review(None).await;
    // A session that cannot be resumed gives way to a new one, which the
    // next review resumes.
    std::fs::write(root.join("stale"), "").unwrap();
    review(None).await;
    std::fs::remove_file(root.join("stale")).unwrap();
    review(None).await;
    let runs = std::fs::read_to_string(root.join("runs")).unwrap();
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

/// What makes [`RESUMER`] resume its sessions as Codex does.
const RESUMES: &str =
    "base = 'codex'\nreview_resume = ['resume', '{session}', '{prompt}', '{schema}']\n";

#[tokio::test]
async fn a_review_runs_beside_a_run_that_could_not_be_removed_in_a_new_session() {
    let fixture = fixture_with(&[("fake", RESUMER)], "agent = 'fake'", RESUMES).await;
    let manager = &fixture.manager;
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
    (manager.reviews)
        .hold_checkout(kept, &repository, &run)
        .keep();
    let beside = review().await;
    assert!(run.exists());
    // Once it is gone, the next review continues the session there.
    manager.reviews.checkouts.lock().unwrap().remove(&kept);
    std::fs::remove_dir_all(&run).unwrap();
    review().await;
    let root = fixture._root.path();
    let runs = std::fs::read_to_string(root.join("runs")).unwrap();
    assert_eq!(
        runs.lines().collect::<Vec<_>>(),
        ["new", "new", "resume s1"]
    );
    let cwd = std::fs::read_to_string(root.join("cwd")).unwrap();
    let cwd: Vec<_> = cwd.lines().map(std::path::Path::new).collect();
    let workspace = fixture.workspace.id.as_str();
    let elsewhere = std::path::Path::new(&format!("{workspace}-{}", beside.id)).join("checkout");
    assert!(cwd[1].ends_with(&elsewhere), "{cwd:?}");
    assert_eq!(cwd[0], cwd[2]);
}

#[tokio::test]
async fn a_review_queued_while_a_cancelled_one_ends_waits_for_its_run() {
    let fixture = fixture(&[("slow", &until_tests_end())], "agent = 'slow'").await;
    let manager = &fixture.manager;
    let selector = SpaceSelector::id(&fixture.workspace.id);
    let queue =
        || manager.queue_review(&selector, ReviewTarget::Workspace, None, Default::default());
    let first = queue().await.unwrap();
    let mut changed = manager.store.watch_changes();
    let state = |id| async move { manager.review(id).await.unwrap().state };
    while state(first.id).await != ReviewState::Running {
        changed.changed().await.unwrap();
    }
    manager
        .cancel_review(None, Some(first.id), None)
        .await
        .unwrap();
    let second = queue().await.unwrap();
    while state(second.id).await == ReviewState::Queued {
        changed.changed().await.unwrap();
    }
    assert_eq!(state(second.id).await, ReviewState::Running);
    // It holds the workspace's place, which it takes before it starts, not
    // one beside the cancelled run.
    let held = manager.reviews.checkouts.lock().unwrap()[&second.id]
        .run
        .clone();
    assert_eq!(held, fixture.run());
}

#[tokio::test]
async fn a_run_left_in_the_repository_directory_holds_back_its_move_and_removal() {
    let fixture = fixture(&[], "").await;
    let manager = &fixture.manager;
    let repository = fixture.workspace.repository().clone();
    fixture.runs.create().unwrap();
    manager.ensure_no_reviews_in(&repository).await.unwrap();
    let run = fixture.left_run(ReviewId(7));
    std::fs::create_dir_all(run.join("checkout")).unwrap();
    assert!(manager.ensure_no_reviews_in(&repository).await.is_err());
    assert!(manager.remove_repository(&repository).await.is_err());
    assert!(run.exists());
    std::fs::remove_dir_all(&run).unwrap();
    manager.ensure_no_reviews_in(&repository).await.unwrap();
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
    let manager = &fixture.manager;
    let repository = fixture.workspace.repository().clone();
    let id = fixture.workspace.id.clone();
    let stuck = manager
        .store
        .run(move |db| {
            let review = store::queue(
                db,
                &id,
                &ReviewTarget::Workspace,
                &"absent".into(),
                &Default::default(),
                UnixTime::from_seconds(1),
            )?
            .id();
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
        .unwrap();
    let run = fixture.left_run(stuck);
    std::fs::create_dir_all(run.join("checkout").join("file")).unwrap();
    // Without write access to the run, its checkout cannot go.
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o500)).unwrap();
    manager.recover_reviews().await.unwrap();
    let workspace = &fixture.workspace.id;
    let delivered = manager
        .agent_messages(&SpaceSelector::from(workspace.as_str()))
        .await
        .unwrap()
        .len();
    assert_eq!(delivered, 1);
    // A later start that still cannot remove it delivers the review once.
    manager.recover_reviews().await.unwrap();
    let messages = manager
        .agent_messages(&SpaceSelector::from(workspace.as_str()))
        .await
        .unwrap();
    assert_eq!(messages.len(), delivered);
    // Nothing runs there, so only the checkout is held, not a slot.
    assert!(manager.agent_queue.admitted_oneshots().is_empty());
    assert!(manager.ensure_no_reviews_in(&repository).await.is_err());
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    let review = manager.review(stuck).await.unwrap();
    assert_eq!(review.state, ReviewState::Failed, "{review:?}");
    assert!(run.join("checkout").exists());
    // The next start removes it.
    manager.recover_reviews().await.unwrap();
    assert!(!run.exists());
}

#[tokio::test]
async fn a_run_recovery_cannot_stop_keeps_its_checkout_and_admission() {
    let fixture = fixture(&[], "").await;
    let manager = &fixture.manager;
    let repository = fixture.workspace.repository().clone();
    let id = fixture.workspace.id.clone();
    let stuck = manager
        .store
        .run(move |db| {
            let review = store::queue(
                db,
                &id,
                &ReviewTarget::Workspace,
                &"absent".into(),
                &Default::default(),
                UnixTime::from_seconds(1),
            )?
            .id();
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
        .unwrap();
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
    manager
        .oneshot_runs
        .fail_inventory
        .store(true, std::sync::atomic::Ordering::SeqCst);
    manager.recover_reviews().await.unwrap();
    let review = manager.review(stuck).await.unwrap();
    assert_eq!(review.state, ReviewState::Failed, "{review:?}");
    assert!(runs.iter().all(|run| run.join("checkout").exists()));
    // Until a restart, the repository stays held and the slots used.
    let held = manager.reviews.checkouts.lock().unwrap().clone();
    let both = [stuck, gone].into_iter().zip(&runs).map(|(id, run)| {
        let held = HeldCheckout {
            repository: repository.clone(),
            run: run.clone(),
            kept: true,
        };
        (id, held)
    });
    assert_eq!(held, both.collect());
    let slots = manager.agent_queue.admitted_oneshots();
    assert_eq!(slots.len(), 2);
    assert!(slots.iter().all(|(held, _)| *held == repository));
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

#[tokio::test]
async fn a_pr_review_reads_the_head_its_workspace_moved_to_while_it_waited() {
    let (root, manager) = manager().await;
    let result = root.path().join("review.json");
    std::fs::write(&result, REVIEW).unwrap();
    let script = root.path().join("reviewer.sh");
    install_test_script(&script, format!("#!/bin/sh\ncat '{}'\n", result.display()));
    configure(
        &manager,
        &format!(
            "[review]\nagent = 'reviewer'\n[agents.reviewer]\ncommand = ['{}']\nreview = ['{{prompt}}', '{{schema}}']\n\
             [machine]\nmax_heavy_agents = 1\n[machine.overload.disk]\nenabled = false\n",
            script.display()
        ),
    )
    .await;
    let path = repository(root.path(), "repo");
    crate::daemon::test_support::git(&path, &["update-ref", "refs/remotes/origin/main", "main"]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_commit_workspace(
            (&repo.id).into(),
            None,
            crate::protocol::CommitStart::Revision(None),
            Default::default(),
        )
        .await
        .unwrap();
    tokio::spawn(run(manager.clone()));
    tokio::spawn(crate::daemon::queued_starts::run(manager.clone()));
    // No admission pass runs while the test holds the gate, so the review
    // waits for one.
    let gate = manager.agent_queue.lock_admission().await;
    let mut ended = manager.reviews.ended.subscribe();
    let target = ReviewTarget::Pr {
        url: "https://forge.example/team/repo/pulls/8".parse().unwrap(),
        head: "topic".into(),
        base: "main".into(),
        post: false,
    };
    let queued = manager
        .queue_review(
            &SpaceSelector::id(&workspace.id),
            target,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    // The review read the workspace and waits for admission.
    while manager.agent_queue.waiting_oneshots().is_empty() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // Meanwhile the PR's head moves, and its workspace with it.
    std::fs::write(workspace.path.join("moved"), "moved\n").unwrap();
    crate::daemon::test_support::commit(&workspace.path, "moved");
    let moved = crate::daemon::test_support::git(&workspace.path, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let (id, commit) = (workspace.id.clone(), moved.clone());
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET start_commit=?2 WHERE id=?1",
                rusqlite::params![id, commit],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    drop(gate);
    manager.agent_queue.wake();
    ended.changed().await.unwrap();
    let review = manager.await_review(queued.id).await.unwrap();
    assert_eq!(review.head, Some(moved.as_str().into()), "{review:?}");
}
