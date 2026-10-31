use std::sync::Arc;

use super::*;
use crate::{
    reviews::{FailureReason, ReviewState, Severity},
    test_support::{configure, install_test_script, manager, repository},
};

const REVIEW: &str = r#"{"summary":"One problem.","verdict":"comment","checked":[],
"findings":[{"path":"tracked","line":1,"severity":"should-fix","title":"Unclear","body":"Name it."}]}"#;

struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    workspace: Workspace,
}

/// A workspace whose reviewers are `[agents.<name>]` scripts, in which
/// `{review}` names a file holding [`REVIEW`], with the review scheduler
/// running when there are any.
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
    }
    Fixture {
        _root: root,
        manager,
        workspace,
    }
}

impl Fixture {
    async fn review(&self) -> Review {
        let queued = self
            .manager
            .queue_review(&self.workspace.id, ReviewTarget::Workspace, None)
            .await
            .unwrap();
        self.manager.await_review(queued.id).await.unwrap()
    }
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
async fn cancelling_stops_the_reviewer_and_removes_its_checkout() {
    let fixture = fixture(&[("slow", "exec sleep 100000")], "agent = 'slow'").await;
    let manager = &fixture.manager;
    let queued = manager
        .queue_review(&fixture.workspace.id, ReviewTarget::Workspace, None)
        .await
        .unwrap();
    let mut changed = manager.store.watch_changes();
    while manager.review(queued.id).await.unwrap().state != ReviewState::Running {
        changed.changed().await.unwrap();
    }
    let mut ended = manager.reviews.ended.subscribe();
    let cancelled = manager
        .cancel_review(
            Some(&fixture.workspace.id),
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
            &fixture.workspace.id,
            ReviewTarget::Workspace,
            Some("pi".parse().unwrap()),
        )
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("agents.pi.review"),
        "{refused}"
    );
    let unset = fixture
        .manager
        .queue_review(&fixture.workspace.id, ReviewTarget::Workspace, None)
        .await
        .unwrap_err();
    assert!(unset.to_string().contains("review.agent"), "{unset}");
}

#[tokio::test]
async fn startup_removes_interrupted_checkouts_and_queues_their_reviews_again() {
    let fixture = fixture(&[], "").await;
    let manager = &fixture.manager;
    let id = fixture.workspace.id.clone();
    let queued = manager
        .store
        .run(move |db| store::queue(db, &id, &ReviewTarget::Workspace, "absent", 1))
        .await
        .unwrap()
        .id();
    manager
        .store
        .run(move |db| store::start(db, queued, "head", "base", 2))
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
    manager.recover_reviews().await.unwrap();
    assert!(!checkout.parent().unwrap().exists());
    let review = manager.review(queued).await.unwrap();
    assert_eq!((review.state, review.head), (ReviewState::Queued, None));
}

#[tokio::test]
async fn removing_the_workspace_stops_its_review() {
    let fixture = fixture(&[("slow", "exec sleep 100000")], "agent = 'slow'").await;
    let manager = &fixture.manager;
    let queued = manager
        .queue_review(&fixture.workspace.id, ReviewTarget::Workspace, None)
        .await
        .unwrap();
    let mut changed = manager.store.watch_changes();
    while manager.review(queued.id).await.unwrap().state != ReviewState::Running {
        changed.changed().await.unwrap();
    }
    let mut ended = manager.reviews.ended.subscribe();
    manager
        .remove_workspace(
            &fixture.workspace.id,
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
        .resolve_finding(&fixture.workspace.id, finding, Some(Outcome::Fixed))
        .await
        .unwrap();
    fixture.review().await;
    let prompt_file = fixture._root.path().join("prompt");
    let prompt = std::fs::read_to_string(prompt_file).unwrap();
    assert!(
        prompt.starts_with("You are reviewing the committed changes of Shoal workspace fix-login")
    );
    assert!(
        prompt.contains(&format!(
            "### Finding {finding} (should-fix) at tracked:1: Unclear"
        )),
        "{prompt}"
    );
    assert!(
        prompt.contains("Outcome: fixed; check it and report it in `checked`"),
        "{prompt}"
    );
}
