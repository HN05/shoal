use crate::time::UnixTime;
use std::sync::Arc;

use super::*;
use crate::client::space_names::SpaceSelector;
use crate::model::Workspace;
use crate::{
    daemon::test_support::{configure, install_test_script, manager, repository, until_tests_end},
    reviews::{FailureReason, ReviewState, Severity},
};

const REVIEW: &str = r#"{"summary":"One problem.","verdict":"comment","checked":[],
"findings":[{"path":"tracked","line":1,"severity":"should-fix","title":"Unclear","body":"Name it."}]}"#;

struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    workspace: Workspace,
}

/// A workspace whose reviewers are `[agents.<name>]` scripts, in which
/// `{review}` names a file holding [`REVIEW`], with the review scheduler and
/// the agent queue, under a fixed agent limit, running when there are any.
async fn fixture(reviewers: &[(&str, &str)], review: &str) -> Fixture {
    let (root, manager) = manager().await;
    let result = root.path().join("review.json");
    std::fs::write(&result, REVIEW).unwrap();
    let mut config = format!("[review]\n{review}\n");
    for (name, script) in reviewers {
        let path = root.path().join(format!("{name}.sh"));
        let script = script.replace("{review}", &result.to_string_lossy());
        install_test_script(&path, &format!("#!/bin/sh\n{script}\n"));
        config.push_str(&format!(
            "[agents.{name}]\ncommand = ['{}']\nreview = ['{{prompt}}', '{{schema}}']\n",
            path.display()
        ));
    }
    config.push_str("[machine]\nmax_agents = 4\n[machine.overload.disk]\nenabled = false\n");
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
    }
}

impl Fixture {
    /// A review of the workspace, once its run ended and delivered it.
    async fn review(&self) -> Review {
        self.review_of(&self.workspace.id).await
    }

    async fn review_of(&self, workspace: &str) -> Review {
        let mut ended = self.manager.reviews.ended.subscribe();
        let queued = self
            .manager
            .queue_review(
                &SpaceSelector::parse(workspace),
                ReviewTarget::Workspace,
                None,
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
    assert_eq!(review.reviewer.as_deref(), Some("fake"));
    let head = crate::git::run(&fixture.workspace.path, &["rev-parse", "HEAD"])
        .await
        .unwrap();
    assert_eq!(review.head.as_deref(), Some(head.trim()));
    assert!(review.base.is_some());
    assert_eq!(review.outdated, Some(false));
    assert_eq!(review.findings[0].severity, Severity::ShouldFix);
    assert_eq!(
        review.findings[0].id.to_string(),
        format!("{}.1", review.id)
    );
    assert!(
        !fixture
            .manager
            .paths
            .reviews()
            .join(review.id.to_string())
            .exists()
    );
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
    assert_eq!(review.reviewer.as_deref(), Some("first"));

    configure(&fixture.manager, "[review]\nagent = 'first'\nfallback_agent = 'second'\n[agents.first]\ncommand = ['false']\nreview = ['{prompt}', '{schema}']\n[agents.second]\ncommand = ['echo']\nreview = ['{prompt}', '{schema}']\n").await;
    let review = fixture.review().await;
    assert_eq!(review.state, ReviewState::Failed);
    assert_eq!(review.failure, Some(FailureReason::InvalidOutput));
    assert_eq!(
        (review.agent.as_str(), review.reviewer.as_deref()),
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
    assert_eq!(review.reviewer.as_deref(), Some("second"));
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
    assert!(!manager.paths.reviews().join(queued.id.to_string()).exists());
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
                "absent",
                &Default::default(),
                UnixTime::from_seconds(1),
            )
        })
        .await
        .unwrap()
        .id();
    manager
        .store
        .run(move |db| store::start(db, queued, "head", "base", UnixTime::from_seconds(2)))
        .await
        .unwrap();
    let checkout = manager
        .paths
        .reviews()
        .join(queued.to_string())
        .join("checkout");
    std::fs::create_dir_all(checkout.parent().unwrap()).unwrap();
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
    let repository = fixture.workspace.repository_id.as_str().to_owned();
    let id = fixture.workspace.id.clone();
    let queued = manager
        .store
        .run(move |db| {
            store::queue(
                db,
                &id,
                &ReviewTarget::Workspace,
                "absent",
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
    let checkout = manager.reviews.hold_checkout(queued, &repository);
    manager
        .store
        .run(move |db| Ok(db.execute("DELETE FROM reviews WHERE id=?1", [queued])?))
        .await
        .unwrap();
    assert!(manager.ensure_no_reviews_in(&repository).await.is_err());
    drop(checkout);
    manager.ensure_no_reviews_in(&repository).await.unwrap();
    // A checkout its run could not remove waits for a restarted daemon.
    manager.reviews.hold_checkout(queued, &repository).keep();
    assert!(manager.ensure_no_reviews_in(&repository).await.is_err());
}

#[tokio::test]
async fn a_replaced_checkout_is_not_reviewed() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let repository = fixture
        .manager
        .repository(fixture.workspace.repository_id.as_str())
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
            fixture.workspace.branch.as_deref().unwrap(),
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
        .remove_workspace(
            &SpaceSelector::id(&fixture.workspace.id),
            crate::removal::UnsavedWork::All,
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    ended.changed().await.unwrap();
    assert!(manager.review(queued.id).await.is_err());
    assert!(!manager.paths.reviews().join(queued.id.to_string()).exists());
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
    let repository = &fixture.workspace.repository_id;
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
    let repository = &fixture.workspace.repository_id;
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
async fn a_false_positive_reserves_the_directory_of_an_older_registration() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let finding = fixture.review().await.findings[0].id;
    let repository = fixture.workspace.repository_id.clone();
    let id = repository.clone();
    fixture
        .manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE repositories SET workspaces_dir=NULL WHERE id=?1",
                [&id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .manager
            .false_positives_path(&repository)
            .await
            .unwrap(),
        None
    );
    let reason = "The field's type names it.".to_owned();
    fixture
        .manager
        .resolve_finding(finding, Some(Outcome::FalsePositive { reason }))
        .await
        .unwrap();
    let path = fixture
        .manager
        .false_positives_path(&repository)
        .await
        .unwrap()
        .unwrap();
    let kept = std::fs::read_to_string(path).unwrap();
    assert!(kept.contains("The field's type names it."), "{kept}");
}

#[tokio::test]
async fn an_invalid_false_positives_file_fails_reviews_of_the_repository() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let repository = &fixture.workspace.repository_id;
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
                &SpaceSelector::parse(workspace),
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
    assert!(line.starts_with("child: "), "{line}");
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
                    agent: None,
                }),
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
        authorize(manager, Some("scope"), &mut resolve(review))
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
            },
        )
        .await;
    assert!(
        authorize(manager, Some("child-scope"), &mut resolve(&parent_review))
            .await
            .is_err()
    );
    let mut show = ReviewMethod::ShowReview {
        workspace: None,
        id: Some(parent_review.id),
    };
    assert!(
        authorize(manager, Some("child-scope"), &mut show)
            .await
            .is_err()
    );
    let mut own = ReviewMethod::ShowReview {
        workspace: None,
        id: None,
    };
    authorize(manager, Some("child-scope"), &mut own)
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
        "agent = 'fake'\nrequire_for_ready = true",
    )
    .await;
    let manager = &fixture.manager;
    let workspace = &fixture.workspace;
    let head = crate::daemon::links::pr::current_head(workspace)
        .await
        .unwrap();
    manager
        .require_review(workspace, Gate::Pr, &head)
        .await
        .unwrap();

    let mut ended = manager.reviews.ended.subscribe();
    assert!(
        manager
            .require_review(workspace, Gate::Ready, &head)
            .await
            .is_err()
    );
    ended.changed().await.unwrap();

    let review = manager
        .latest_review(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let refused = manager
        .require_review(workspace, Gate::Ready, &head)
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
        .require_review(workspace, Gate::Ready, &head)
        .await
        .unwrap();
    // A workspace process's mark checks the HEAD it records.
    let marks = manager
        .mark_ready(&SpaceSelector::id(&workspace.id), Default::default(), true)
        .await
        .unwrap()
        .marks;
    assert_eq!(marks[0].head, head);
}

/// The frames the daemon answers `method` with, and the JSON their review
/// pieces join into.
async fn serve_pieces(manager: &Arc<Manager>, method: serde_json::Value) -> (usize, String) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let request =
        serde_json::json!({"protocol": crate::protocol::VERSION, "id": 5, "method": method});
    let (client, stream) = tokio::net::UnixStream::pair().unwrap();
    let operation = manager.gates.owned_operation().await;
    let server = crate::daemon::tests::server(manager.clone());
    let served = tokio::spawn(crate::daemon::serve(stream, server, operation));
    let mut client = BufReader::new(client);
    client
        .get_mut()
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let (mut frames, mut json) = (0, String::new());
    loop {
        let mut line = String::new();
        client.read_line(&mut line).await.unwrap();
        assert!(line.len() <= crate::protocol::MAX_FRAME, "{}", line.len());
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        match response["type"].as_str() {
            Some("review_pieces") => {
                frames += 1;
                for piece in response["data"].as_array().unwrap() {
                    json.push_str(piece.as_str().unwrap());
                }
            }
            Some("ok") => break,
            _ => panic!("{response}"),
        }
    }
    drop(client);
    served.await.unwrap().unwrap();
    (frames, json)
}

/// Finish review after review of the workspace, each with one finding
/// whose body is `body`, returning their IDs.
async fn finished_reviews(
    manager: &Manager,
    workspace: &str,
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
                    "absent",
                    &Default::default(),
                    UnixTime::from_seconds(1),
                )?
                .id();
                store::start(tx, review, "head", "base", UnixTime::from_seconds(2))?;
                let output: crate::reviews::output::ReviewerOutput = serde_json::from_value(
                    serde_json::json!({"summary": "Long.", "verdict": "comment", "checked": [],
                        "findings": [{"path": "a", "line": 1, "severity": "nit",
                            "title": "Long", "body": body}]}),
                )?;
                store::finish(tx, review, "absent", &output, UnixTime::from_seconds(3))?;
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
        .remove_repository(fixture.workspace.repository_id.as_str())
        .await
        .unwrap();
    assert!(!manager.paths.reviews().join(queued.id.to_string()).exists());
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
    let repository = fixture.workspace.repository_id.as_str().to_owned();
    let id = fixture.workspace.id.clone();
    let stuck = manager
        .store
        .run(move |db| {
            let review = store::queue(
                db,
                &id,
                &ReviewTarget::Workspace,
                "absent",
                &Default::default(),
                UnixTime::from_seconds(1),
            )?
            .id();
            store::start(db, review, "head", "base", UnixTime::from_seconds(2))?;
            Ok(review)
        })
        .await
        .unwrap();
    let run = manager.paths.reviews().join(stuck.to_string());
    std::fs::create_dir_all(run.join("checkout").join("file")).unwrap();
    std::fs::write(run.join("repository"), &repository).unwrap();
    // Without write access to the run, its checkout cannot go.
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o500)).unwrap();
    manager.recover_reviews().await.unwrap();
    let workspace = &fixture.workspace.id;
    let delivered = manager
        .agent_messages(&SpaceSelector::parse(workspace))
        .await
        .unwrap()
        .len();
    assert_eq!(delivered, 1);
    // A later start that still cannot remove it delivers the review once.
    manager.recover_reviews().await.unwrap();
    let messages = manager
        .agent_messages(&SpaceSelector::parse(workspace))
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
async fn a_run_recovery_cannot_stop_keeps_its_checkout_and_slot() {
    let fixture = fixture(&[], "").await;
    let manager = &fixture.manager;
    let repository = fixture.workspace.repository_id.as_str().to_owned();
    let id = fixture.workspace.id.clone();
    let stuck = manager
        .store
        .run(move |db| {
            let review = store::queue(
                db,
                &id,
                &ReviewTarget::Workspace,
                "absent",
                &Default::default(),
                UnixTime::from_seconds(1),
            )?
            .id();
            store::start(db, review, "head", "base", UnixTime::from_seconds(2))?;
            Ok(review)
        })
        .await
        .unwrap();
    // The second run's review went with its workspace.
    let runs =
        [stuck, ReviewId(stuck.0 + 1)].map(|id| manager.paths.reviews().join(id.to_string()));
    for run in &runs {
        std::fs::create_dir_all(run.join("checkout")).unwrap();
        std::fs::write(run.join("repository"), &repository).unwrap();
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
    let both = [stuck, ReviewId(stuck.0 + 1)].map(|id| {
        let held = HeldCheckout {
            repository: repository.clone(),
            kept: true,
        };
        (id, held)
    });
    assert_eq!(held, std::collections::HashMap::from(both));
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
    install_test_script(&script, &format!("#!/bin/sh\ncat '{}'\n", result.display()));
    configure(
        &manager,
        &format!(
            "[review]\nagent = 'reviewer'\n[agents.reviewer]\ncommand = ['{}']\nreview = ['{{prompt}}', '{{schema}}']\n\
             [machine]\nmax_agents = 1\n[machine.overload.disk]\nenabled = false\n",
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
            repo.id.as_str().into(),
            None,
            crate::protocol::CommitStart::Revision(None),
            Default::default(),
        )
        .await
        .unwrap();
    tokio::spawn(run(manager.clone()));
    tokio::spawn(crate::daemon::queued_starts::run(manager.clone()));
    // The only slot is taken, so the review waits for it.
    let mut held = manager.agent_queue.request_oneshot(&repo.id, false, || {});
    held.admitted().await;
    let mut ended = manager.reviews.ended.subscribe();
    let target = ReviewTarget::Pr {
        url: "https://forge.example/team/repo/pulls/8".into(),
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
    // The review read the workspace and waits for the slot.
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
    drop(held);
    ended.changed().await.unwrap();
    let review = manager.await_review(queued.id).await.unwrap();
    assert_eq!(review.head.as_deref(), Some(moved.as_str()), "{review:?}");
}
