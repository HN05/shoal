use crate::shoal_dirs::{Contents, ShoalDir};
use crate::time::UnixTime;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::BranchName;
use std::sync::Arc;

use super::*;
use crate::model::Workspace;
use crate::{
    daemon::test_support::{configure, install_test_script, manager, repository, until_tests_end},
    reviews::{FailureReason, Review, ReviewId, ReviewState, ReviewTarget},
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_reviews::store;

const REVIEW: &str = r#"{"summary":"One problem.","verdict":"comment","reopened":[],
"findings":[{"path":"tracked","start_line":1,"end_line":1,"severity":"should-fix",
"title":"Unclear","body":"Name it."}]}"#;

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
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    if !reviewers.is_empty() {
        tokio::spawn(shoal_reviews::run(manager.reviews.clone(), manager.clone()));
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
        let mut ended = self.manager.reviews.ends();
        let queued = self
            .manager
            .reviews
            .queue(
                self.manager.as_ref(),
                &SpaceSelector::id(workspace),
                ReviewTarget::Workspace,
                agent.map(crate::agent::Agent::from),
                Default::default(),
            )
            .await
            .unwrap();
        ended.changed().await.unwrap();
        self.manager
            .reviews
            .wait(self.manager.as_ref(), queued.id)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn a_review_protection_stops_is_queued_again() {
    let fixture = fixture(&[("slow", &until_tests_end())], "agent = 'slow'").await;
    let manager = &fixture.manager;
    let queued = manager
        .reviews
        .queue(
            manager.as_ref(),
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let mut changed = manager.store.watch_changes();
    while manager
        .reviews
        .review(manager.as_ref(), queued.id)
        .await
        .unwrap()
        .state
        != ReviewState::Running
    {
        changed.changed().await.unwrap();
    }
    assert!(
        manager
            .stop_for_overload(crate::daemon::overload::Pressure::Memory)
            .await
    );
    while manager
        .reviews
        .review(manager.as_ref(), queued.id)
        .await
        .unwrap()
        .state
        != ReviewState::Queued
    {
        changed.changed().await.unwrap();
    }
    let review = manager
        .reviews
        .review(manager.as_ref(), queued.id)
        .await
        .unwrap();
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
    let mut ended = manager.reviews.ends();
    let cancelled = manager
        .reviews
        .cancel(manager.as_ref(), None, Some(queued.id), None)
        .await
        .unwrap();
    assert_eq!(cancelled.state, ReviewState::Cancelled);
    ended.changed().await.unwrap();
}

#[tokio::test]
async fn a_replaced_checkout_is_not_reviewed() {
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let repository = fixture
        .manager
        .repositories
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
        .reviews
        .queue(
            manager.as_ref(),
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let mut changed = manager.store.watch_changes();
    while manager
        .reviews
        .review(manager.as_ref(), queued.id)
        .await
        .unwrap()
        .state
        != ReviewState::Running
    {
        changed.changed().await.unwrap();
    }
    let mut ended = manager.reviews.ends();
    manager
        .remove_selected(
            &SpaceSelector::id(&fixture.workspace.id),
            crate::removal::UnsavedWork::All,
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    ended.changed().await.unwrap();
    assert!(
        manager
            .reviews
            .review(manager.as_ref(), queued.id)
            .await
            .is_err()
    );
    assert!(!fixture.run().exists());
}

#[tokio::test]
async fn the_coordinating_agent_hears_one_line() {
    use crate::state::AgentState;
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let manager = &fixture.manager;
    let parent = &fixture.workspace;
    let child = manager
        .create_child(
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
        .map(|message| message.text())
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
        .create_child(
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
                    kind: crate::daemon::executions::ExecutionKind::Command,
                    agent_execution: None,
                }),
                granted: None,
                read_only: false,
                vouched: None,
            },
        )
        .await;
    let cancel = |review: &Review| ReviewMethod::CancelReview {
        workspace: None,
        id: Some(review.id),
    };
    // The parent's agent reaches its own and its child's reviews.
    for review in [&parent_review, &child_review] {
        authorize(manager, Some(&"scope".into()), &mut cancel(review))
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
                vouched: None,
            },
        )
        .await;
    assert!(
        authorize(
            manager,
            Some(&"child-scope".into()),
            &mut cancel(&parent_review)
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
async fn a_finding_is_a_thread_its_workspace_resolves_keeping_its_false_positive() {
    use shoal_protocol::threads::{
        OutcomeKind, Resolving, Sender, ThreadKind, ThreadNumber, ThreadStatus,
    };
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspace);
    let review = fixture.review().await;
    let [thread] = review.threads.as_slice() else {
        panic!("{:?}", review.threads);
    };
    assert_eq!(
        (thread.kind, thread.opener),
        (ThreadKind::Review, Sender::Shoal)
    );
    assert_eq!(thread.status(), ThreadStatus::Queued);
    assert_eq!(thread.about().unwrap().original.to_string(), "tracked:1");
    let agent = crate::daemon::scope::Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let resolving = Resolving {
        outcome: Some(OutcomeKind::FalsePositive),
        reason: Some("The type names it.".into()),
        ..Resolving::default()
    };
    let number = ThreadNumber::Full(thread.number);
    (manager.resolve_thread(Some(&agent), number, resolving))
        .await
        .unwrap();
    let path = (manager
        .repositories
        .false_positives_path(workspace.repository()))
    .await
    .unwrap()
    .unwrap();
    let kept = std::fs::read_to_string(&path).unwrap();
    assert!(kept.contains("The type names it."), "{kept}");
    // It names the finding by its review and its place there.
    let finding = format!("finding = \"{}.1\"", review.id);
    assert!(kept.contains(&finding), "{kept}");
    let shown = (manager.reviews.review(manager.as_ref(), review.id))
        .await
        .unwrap();
    assert_eq!(shown.unresolved().count(), 0);

    // Reopening it drops it from the false positives.
    (manager.resolve_thread(Some(&agent), number, Resolving::default()))
        .await
        .unwrap();
    let kept = std::fs::read_to_string(&path).unwrap();
    assert!(!kept.contains("The type names it."), "{kept}");
}

#[tokio::test]
async fn the_reviewer_answers_its_workspace_s_post_in_a_finding_s_thread() {
    use shoal_protocol::threads::{NewPost, Sender, ThreadNumber};
    let script = "case \"$1\" in *'reply in `answer`'*)\n\
        echo '{\"answer\":\"It is reachable from parse.\"}';;\n\
        *) cat '{review}';; esac";
    let fixture = fixture(&[("fake", script)], "agent = 'fake'").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspace);
    let review = fixture.review().await;
    let number = ThreadNumber::Full(review.threads[0].number);
    let agent = crate::daemon::scope::Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let post = NewPost {
        text: "Is this reachable?".into(),
        ..NewPost::default()
    };
    let mut changed = manager.store.watch_changes();
    (manager.post_to_thread(
        Some(&agent),
        number,
        post,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
    .await
    .unwrap();
    let answer = loop {
        let thread = manager.thread(None, number).await.unwrap();
        if let [_, _, answer] = thread.posts.as_slice() {
            break answer.clone();
        }
        changed.changed().await.unwrap();
    };
    assert_eq!(answer.sender, Sender::Shoal);
    assert_eq!(answer.post.text, "It is reachable from parse.");
}

#[tokio::test]
async fn a_later_review_reopens_only_a_fixed_finding_of_its_workspace() {
    use shoal_protocol::threads::{Outcome, OutcomeKind, Resolving, ThreadNumber};
    let fixture = fixture(&[("fake", "cat '{review}'")], "agent = 'fake'").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspace);
    let review = fixture.review().await;
    let number = review.threads[0].number;
    let reopen = |why: &'static str| {
        let workspace = workspace.id.clone();
        manager
            .store
            .write(move |tx| crate::daemon::threads::findings::reopen(tx, &workspace, number, why))
    };
    assert_eq!(reopen("Still.").await.unwrap(), None);
    let fixed = Resolving {
        outcome: Some(OutcomeKind::Fixed),
        ..Resolving::default()
    };
    (manager.resolve_thread(None, ThreadNumber::Full(number), fixed))
        .await
        .unwrap();
    assert!(
        reopen("The empty case still panics.")
            .await
            .unwrap()
            .is_some()
    );
    let thread = (manager.thread(None, ThreadNumber::Full(number)))
        .await
        .unwrap();
    assert!(thread.is_open());
    let last = thread.last_post().unwrap();
    assert!(
        last.post.text.ends_with("The empty case still panics."),
        "{}",
        last.post.text
    );
    // A thread of another kind, or resolved otherwise, stays as it is.
    let wont = Resolving {
        outcome: Some(OutcomeKind::WontFix),
        reason: Some("Rare.".into()),
        ..Resolving::default()
    };
    (manager.resolve_thread(None, ThreadNumber::Full(number), wont))
        .await
        .unwrap();
    assert_eq!(reopen("Still.").await.unwrap(), None);
    let thread = (manager.thread(None, ThreadNumber::Full(number)))
        .await
        .unwrap();
    assert!(matches!(
        thread.resolution.map(|resolution| resolution.outcome),
        Some(Outcome::WontFix { .. })
    ));
}

#[tokio::test]
async fn all_holds_the_users_own_commands_too() {
    use shoal_reviews::required::Gate;
    let fixture = fixture(&[], "require_for_pr = 'all'").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspace);
    let head = shoal_links::pr::current_head(workspace).await.unwrap();
    let refused = manager
        .reviews
        .require(manager.as_ref(), workspace, Gate::Pr, &head, None)
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

/// The frames the daemon answers `method` with, and the JSON their review
/// pieces join into.
async fn serve_pieces(manager: &Arc<Manager>, method: serde_json::Value) -> (usize, String) {
    let pages = crate::daemon::tests::serve_pages(manager, method).await;
    let pieces = pages.iter().flat_map(|page| page.as_array().unwrap());
    let json = pieces.map(|piece| piece.as_str().unwrap()).collect();
    (pages.len(), json)
}

/// Finish review after review of the workspace, each with `findings`
/// findings whose body is `body`, returning their IDs.
async fn finished_reviews(
    manager: &Manager,
    workspace: &SpaceId,
    count: usize,
    findings: usize,
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
                let finding = serde_json::json!({"path": "a", "start_line": 1, "end_line": 1,
                    "severity": "nit", "title": "Long", "body": body});
                let output: crate::reviews::output::ReviewerOutput = serde_json::from_value(
                    serde_json::json!({"summary": "Long.", "verdict": "comment", "reopened": [],
                        "findings": vec![finding; findings]}),
                )?;
                let code = shoal_reviews::daemon::FindingCode {
                    commit: "head".into(),
                    at: shoal_protocol::code::CodeAt {
                        path: "a".into(),
                        lines: Some(shoal_protocol::code::Lines { start: 0, end: 1 }),
                        blob: None,
                    },
                    snippet: None,
                };
                let returned = store::Returned {
                    output: &output,
                    code: &vec![code; findings],
                    session: None,
                };
                let now = UnixTime::from_seconds(3);
                let records = super::ReviewRecords;
                store::finish(tx, &records, review, &id, &"absent".into(), returned, now)?;
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
    finished_reviews(&manager, &fixture.workspace.id, 30, 1, &"x".repeat(4096)).await;
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
    let body = "y".repeat(shoal_protocol::threads::MAX_POST_LEN);
    let id = finished_reviews(&manager, &fixture.workspace.id, 1, 9, &body).await[0];
    let method = serde_json::json!({"show_review": {"id": id}});
    let (frames, json) = serve_pieces(&manager, method).await;
    assert!(frames > 1, "{frames}");
    let review: Review = serde_json::from_str(&json).unwrap();
    assert_eq!(review.threads.len(), 9);
}

#[tokio::test]
async fn removing_the_repository_stops_its_reviews_first() {
    let fixture = fixture(&[("slow", &until_tests_end())], "agent = 'slow'").await;
    let manager = &fixture.manager;
    let queued = manager
        .reviews
        .queue(
            manager.as_ref(),
            &SpaceSelector::id(&fixture.workspace.id),
            ReviewTarget::Workspace,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let mut changed = manager.store.watch_changes();
    while manager
        .reviews
        .review(manager.as_ref(), queued.id)
        .await
        .unwrap()
        .state
        != ReviewState::Running
    {
        changed.changed().await.unwrap();
    }
    manager
        .repositories
        .remove(&**manager, fixture.workspace.repository())
        .await
        .unwrap();
    assert!(!fixture.runs.root().exists());
}

#[tokio::test]
async fn a_run_left_in_the_repository_directory_holds_back_its_removal() {
    let fixture = fixture(&[], "").await;
    let manager = &fixture.manager;
    let repository = fixture.workspace.repository().clone();
    let run = fixture.left_run(ReviewId(7));
    std::fs::create_dir_all(run.join("checkout")).unwrap();
    assert!(
        manager
            .repositories
            .remove(&**manager, &repository)
            .await
            .is_err()
    );
    assert!(run.exists());
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
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_commit_workspace(
            (&repo.id).into(),
            (None, None),
            crate::protocol::CommitStart::Revision(None),
            Default::default(),
        )
        .await
        .unwrap();
    tokio::spawn(shoal_reviews::run(manager.reviews.clone(), manager.clone()));
    tokio::spawn(crate::daemon::queued_starts::run(manager.clone()));
    // No admission pass runs while the test holds the gate, so the review
    // waits for one.
    let gate = manager.agent_queue.lock_admission().await;
    let mut ended = manager.reviews.ends();
    let target = ReviewTarget::Pr {
        url: "https://forge.example/team/repo/pulls/8".parse().unwrap(),
        head: "topic".into(),
        base: "main".into(),
        post: false,
    };
    let queued = manager
        .reviews
        .queue(
            manager.as_ref(),
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
    let review = manager
        .reviews
        .wait(manager.as_ref(), queued.id)
        .await
        .unwrap();
    assert_eq!(review.head, Some(moved.as_str().into()), "{review:?}");
}
