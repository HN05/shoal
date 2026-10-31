use std::sync::Arc;

use super::*;
use crate::model::Workspace;
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
    /// A review of the workspace, once its run ended and delivered it.
    async fn review(&self) -> Review {
        self.review_of(&self.workspace.id).await
    }

    async fn review_of(&self, workspace: &str) -> Review {
        let mut ended = self.manager.reviews.ended.subscribe();
        let queued = self
            .manager
            .queue_review(workspace, ReviewTarget::Workspace, None)
            .await
            .unwrap();
        ended.changed().await.unwrap();
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
    // A reviewer that outlived its daemon, and someone's shell in the
    // checkout, neither a child of this process.
    let detached = |marked: bool| {
        // This test binary, whose environment can be read, unlike that of
        // the platform's own executables on macOS.
        let mut command = std::process::Command::new("sh");
        command
            .args([
                "-c",
                "\"$0\" --exact daemon::reviews::tests::sleep_as_a_survivor --ignored >/dev/null 2>&1 & echo $!",
            ])
            .arg(std::env::current_exe().unwrap())
            .current_dir(&checkout);
        if marked {
            command.env(crate::env::REVIEW_RUN, checkout.parent().unwrap());
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
    let (survivor, bystander) = (identity(detached(true)), identity(detached(false)));
    manager.recover_reviews().await.unwrap();
    // Alive as recovery judges it: a stopped orphan may wait to be reaped.
    let alive = |process| crate::process::identity::alive(process).unwrap();
    assert!(!alive(&survivor), "the survivor runs on");
    assert!(alive(&bystander), "the bystander was stopped");
    crate::process::identity::stop_verified(&[bystander])
        .await
        .unwrap();
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
        .resolve_finding(finding, Some(Outcome::Fixed))
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

#[tokio::test]
async fn the_workspace_hears_its_reviews_except_its_own_cancels() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let manager = &fixture.manager;
    let review = fixture.review().await;
    let messages = manager.agent_messages(&fixture.workspace.id).await.unwrap();
    let delivered = &messages.last().unwrap().message;
    assert!(
        delivered.starts_with(&format!("Review {} finished: 1 should-fix.", review.id)),
        "{delivered}"
    );

    configure(manager, "[review]\nagent = 'fake'\n[agents.fake]\ncommand = ['sleep']\nreview = ['100000', '{prompt}', '{schema}']\n").await;
    let queued = manager
        .queue_review(&fixture.workspace.id, ReviewTarget::Workspace, None)
        .await
        .unwrap();
    let before = manager
        .agent_messages(&fixture.workspace.id)
        .await
        .unwrap()
        .len();
    manager
        .cancel_review(None, Some(queued.id), Some(fixture.workspace.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        manager
            .agent_messages(&fixture.workspace.id)
            .await
            .unwrap()
            .len(),
        before
    );
    let queued = manager
        .queue_review(&fixture.workspace.id, ReviewTarget::Workspace, None)
        .await
        .unwrap();
    manager
        .cancel_review(None, Some(queued.id), None)
        .await
        .unwrap();
    let messages = manager.agent_messages(&fixture.workspace.id).await.unwrap();
    assert!(
        messages
            .last()
            .unwrap()
            .message
            .starts_with(&format!("Review {} was cancelled.", queued.id))
    );
}

#[tokio::test]
async fn the_coordinating_agent_hears_one_line() {
    use crate::state::AgentState;
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let manager = &fixture.manager;
    let parent = &fixture.workspace;
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    for workspace in [&parent.id, &child.id] {
        manager
            .set_agent_state(workspace, AgentState::Working, None, None)
            .await
            .unwrap();
    }
    let queued = fixture.review_of(&child.id).await;
    let received: Vec<String> = manager
        .agent_messages(&parent.id)
        .await
        .unwrap()
        .into_iter()
        .map(|message| message.message)
        .collect();
    assert_eq!(
        received,
        [format!(
            "child: review {} finished: 1 should-fix",
            queued.id
        )]
    );
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
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    let parent_review = fixture.review().await;
    let child_review = fixture.review_of(&child.id).await;
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: parent.id.clone(),
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
                workspace_id: child.id.clone(),
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
    assert!(matches!(own, ReviewMethod::ShowReview { workspace: Some(id), .. } if id == child.id));
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
    let refused = manager
        .require_review(workspace, Gate::Ready, &head)
        .await
        .unwrap_err();
    let refused = refused.to_string();
    assert!(
        refused.contains("marking work ready needs a finished review of HEAD"),
        "{refused}"
    );
    assert!(
        refused.contains("whose findings arrive as a message"),
        "{refused}"
    );
    ended.changed().await.unwrap();

    let review = manager.latest_review(&workspace.id).await.unwrap();
    let refused = manager
        .require_review(workspace, Gate::Ready, &head)
        .await
        .unwrap_err();
    let refused = refused.to_string();
    assert!(
        refused.contains(&format!(
            "review {} of HEAD has unresolved findings",
            review.id
        )),
        "{refused}"
    );
    assert!(
        refused.contains(&format!(
            "{} should-fix at tracked:1: Unclear",
            review.findings[0].id
        )),
        "{refused}"
    );
    // Unresolved findings at HEAD need answers, not another review.
    assert_eq!(
        manager.latest_review(&workspace.id).await.unwrap().id,
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
        .mark_ready(&workspace.id, Default::default(), true)
        .await
        .unwrap();
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
) -> Vec<i64> {
    let (id, body) = (workspace.to_owned(), body.to_owned());
    manager
        .store
        .run(move |db| {
            let mut ids = Vec::new();
            for _ in 0..count {
                let review = store::queue(db, &id, &ReviewTarget::Workspace, "absent", 1)?.id();
                store::start(db, review, "head", "base", 2)?;
                let output: crate::reviews::output::ReviewerOutput = serde_json::from_value(
                    serde_json::json!({"summary": "Long.", "verdict": "comment", "checked": [],
                        "findings": [{"path": "a", "line": 1, "severity": "nit",
                            "title": "Long", "body": body}]}),
                )?;
                store::finish(db, review, "absent", &output, 3)?;
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
    let id = fixture.workspace.id.clone();
    let stuck = manager
        .store
        .run(move |db| {
            let review = store::queue(db, &id, &ReviewTarget::Workspace, "absent", 1)?.id();
            store::start(db, review, "head", "base", 2)?;
            Ok(review)
        })
        .await
        .unwrap();
    let run = manager.paths.reviews().join(stuck.to_string());
    std::fs::create_dir_all(run.join("checkout").join("file")).unwrap();
    // Without write access to the run, its checkout cannot go.
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o500)).unwrap();
    manager.recover_reviews().await.unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    let review = manager.review(stuck).await.unwrap();
    assert_eq!(review.state, ReviewState::Failed, "{review:?}");
    assert!(run.join("checkout").exists());
    // The next start removes it.
    manager.recover_reviews().await.unwrap();
    assert!(!run.exists());
}

/// Sleeps as a process a stopped daemon's reviewer left, for the recovery
/// test, which starts it.
#[test]
#[ignore]
fn sleep_as_a_survivor() {
    std::thread::sleep(std::time::Duration::from_secs(100_000));
}
