use super::*;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        approvals::proposals::PR,
        test_support::{commit, manager, repository},
    },
    forge::items::create::NewPull,
};

/// A workspace whose repository holds its agents' pushes back, and its
/// agent as a scoped caller.
async fn setup() -> (
    tempfile::TempDir,
    std::sync::Arc<Manager>,
    Workspace,
    Caller,
) {
    setup_with("propose").await
}

/// A workspace whose repository sets `[permissions] push` to `push`.
async fn setup_with(
    push: &str,
) -> (
    tempfile::TempDir,
    std::sync::Arc<Manager>,
    Workspace,
    Caller,
) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    std::fs::write(
        workspace.path.join(".shoal.toml"),
        format!("[permissions]\npush = '{push}'\n"),
    )
    .unwrap();
    commit(&workspace.path, ".shoal.toml");
    let caller = Caller {
        space_id: workspace.id.clone(),
        execution: None,
    };
    (root, manager, workspace, caller)
}

async fn branch(workspace: &Workspace) -> PushSpecification {
    branch_push(workspace, None).await.unwrap()
}

fn proposed(outcome: PushOutcome) -> ApprovalRequest {
    match outcome {
        PushOutcome::Proposed(proposed) => *proposed.proposed,
        PushOutcome::Direct => panic!("expected the push to wait for approval"),
    }
}

#[tokio::test]
async fn only_scoped_pushes_wait_where_the_repository_asks() {
    let (_root, manager, workspace, caller) = setup().await;
    let push = branch(&workspace).await;
    let outcome = manager
        .request_push(&SpaceSelector::id(&workspace.id), push.clone(), None)
        .await
        .unwrap();
    assert!(matches!(outcome, PushOutcome::Direct));

    let request = proposed(
        manager
            .request_push(
                &SpaceSelector::id(&workspace.id),
                push.clone(),
                Some(&caller),
            )
            .await
            .unwrap(),
    );
    assert_eq!(request.target, Target::Push);
    assert_eq!(request.name, "refs/heads/worker");
    let subject = request.subject();
    assert!(
        subject.contains(push.commit.as_deref().unwrap()),
        "{subject}"
    );
    // Pushing the same again names the same request.
    let again = proposed(
        manager
            .request_push(
                &SpaceSelector::id(&workspace.id),
                push.clone(),
                Some(&caller),
            )
            .await
            .unwrap(),
    );
    assert_eq!(again.key, request.key);
    // A pending push keeps the workspace from unattended removal.
    assert!(manager.has_pending_push(&workspace.id).await.unwrap());

    // A newer commit replaces it, and the old request stays outdated.
    std::fs::write(workspace.path.join("tracked"), "changed\n").unwrap();
    commit(&workspace.path, "tracked");
    let newer = proposed(
        manager
            .request_push(
                &SpaceSelector::id(&workspace.id),
                branch(&workspace).await,
                Some(&caller),
            )
            .await
            .unwrap(),
    );
    assert_ne!(newer.key, request.key);
    let old = manager
        .approval_by_key(&request.key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.status, DecisionStatus::Outdated);
}

#[tokio::test]
async fn a_pr_proposal_that_pushes_takes_the_place_of_its_push_request() {
    let (_root, manager, workspace, caller) = setup().await;
    let push = branch(&workspace).await;
    let separate = proposed(
        manager
            .request_push(
                &SpaceSelector::id(&workspace.id),
                push.clone(),
                Some(&caller),
            )
            .await
            .unwrap(),
    );
    let pull = NewPull {
        title: "Fix".into(),
        head: workspace.branch.clone().unwrap(),
        ..NewPull::default()
    };
    let commit = push.commit.clone().unwrap();
    let pull = manager
        .propose_pull(
            &workspace,
            pull,
            commit.clone(),
            Some(push.clone()),
            Default::default(),
        )
        .await
        .unwrap()
        .proposed;
    assert_eq!(pull.name, PR);
    let subject = pull.subject();
    assert!(
        subject.contains("Fix") && subject.contains(&commit),
        "{subject}"
    );
    let retired = manager
        .approval_by_key(&separate.key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retired.status, DecisionStatus::Outdated);
    // Pushing the branch again while the PR waits names the PR's request.
    let again = proposed(
        manager
            .request_push(&SpaceSelector::id(&workspace.id), push, Some(&caller))
            .await
            .unwrap(),
    );
    assert_eq!(again.key, pull.key);
}

#[tokio::test]
async fn approving_a_push_runs_it_and_its_end_decides_the_request() {
    let (_root, manager, workspace, caller) = setup().await;
    let request = proposed(
        manager
            .request_push(
                &SpaceSelector::id(&workspace.id),
                branch(&workspace).await,
                Some(&caller),
            )
            .await
            .unwrap(),
    );
    let approve = || manager.decide_approval(request.number, true, None, Default::default(), None);
    let running = approve().await.unwrap();
    assert_eq!(running.status, DecisionStatus::Pending);
    assert!(running.operation.is_some());
    // The push lands whatever happens to the request, so nothing else
    // decides, withdraws or replaces it while the push runs.
    assert!(approve().await.is_err());
    let denied = manager.decide_approval(request.number, false, None, Default::default(), None);
    assert!(denied.await.is_err());
    let selected = SpaceSelector::id(&workspace.id);
    let withdrawn = manager.withdraw_approval(&selected, request.number);
    assert!(withdrawn.await.is_err());
    std::fs::write(workspace.path.join("tracked"), "changed\n").unwrap();
    commit(&workspace.path, "tracked");
    let selected = SpaceSelector::id(&workspace.id);
    let replaced = manager.request_push(&selected, branch(&workspace).await, Some(&caller));
    assert!(replaced.await.is_err());

    // A failed push leaves the request to approve again.
    manager
        .finish_approved_push(&request.key, Some("rejected".into()))
        .await
        .unwrap_err();
    let kept = manager
        .approval_by_key(&request.key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.status, DecisionStatus::Pending);
    assert!(kept.operation.is_none());

    let decided = manager
        .finish_approved_push(&request.key, None)
        .await
        .unwrap();
    assert_eq!(decided.status, DecisionStatus::Approved);
    assert!(
        manager
            .approval_by_key(&request.key)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        manager
            .agent_messages(&"worker".into())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn approving_a_push_of_a_branch_that_moved_outdates_it() {
    let (_root, manager, workspace, caller) = setup().await;
    let request = proposed(
        manager
            .request_push(
                &SpaceSelector::id(&workspace.id),
                branch(&workspace).await,
                Some(&caller),
            )
            .await
            .unwrap(),
    );
    std::fs::write(workspace.path.join("tracked"), "changed\n").unwrap();
    commit(&workspace.path, "tracked");
    manager
        .decide_approval(request.number, true, None, Default::default(), None)
        .await
        .unwrap_err();
    let outdated = manager
        .approval_by_key(&request.key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outdated.status, DecisionStatus::Outdated);
    assert!(outdated.operation.is_none());
    assert_eq!(
        manager
            .agent_messages(&"worker".into())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn deny_refuses_scoped_pushes_and_lets_the_user_push() {
    let (root, manager, workspace, caller) = setup_with("deny").await;
    let remote = root.path().join("remote.git");
    crate::daemon::test_support::git(root.path(), &["init", "-q", "--bare", "remote.git"]);
    crate::daemon::test_support::git(
        &workspace.path,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    let push = branch(&workspace).await;
    let selected = SpaceSelector::id(&workspace.id);
    let requested = manager.request_push(&selected, push.clone(), Some(&caller));
    assert!(requested.await.is_err());
    let selected = SpaceSelector::id(&workspace.id);
    let held = manager.branch_push_held(&selected, Some(&caller));
    assert!(held.await.is_err());
    let outcome = manager
        .request_push(&SpaceSelector::id(&workspace.id), push, None)
        .await
        .unwrap();
    assert!(matches!(outcome, PushOutcome::Direct));
    assert!(
        !manager
            .branch_push_held(&SpaceSelector::id(&workspace.id), None)
            .await
            .unwrap()
    );
    assert!(manager.approval_requests(None).await.unwrap().is_empty());
}
