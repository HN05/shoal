use super::*;
use crate::{
    daemon::approvals::proposals::PR,
    test_support::{manager, repository},
};

#[tokio::test]
async fn the_sweep_keeps_only_files_pending_proposals_attach() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let files = manager.paths.proposal_files();
    let kept = files.join("kept/0/shot.png");
    std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
    std::fs::write(&kept, "png").unwrap();
    std::fs::create_dir_all(files.join("stale/0")).unwrap();
    let pull = NewPull {
        attachments: vec![Attachment {
            path: kept.clone(),
            alt: None,
        }],
        ..NewPull::default()
    };
    let specification = Specification::Pull(PullSpecification {
        pull,
        commit: "head".into(),
        agent: None,
        follow_up: None,
    });
    let request = proposal(&workspace, Target::PrOpen, PR, specification);
    manager
        .store_proposal(request, proposals::propose_pull)
        .await
        .unwrap();

    manager.sweep_proposal_files().await.unwrap();
    assert!(kept.exists());
    assert!(!files.join("stale").exists());

    // Removing the workspace takes its PR proposal, and the next sweep
    // its files.
    manager
        .store
        .run(|db| Ok(db.execute("DELETE FROM workspaces", [])?))
        .await
        .unwrap();
    manager.sweep_proposal_files().await.unwrap();
    assert!(!files.join("kept").exists());
}

#[tokio::test]
async fn a_decision_refuses_a_proposal_changed_while_it_waited() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let issue = NewIssue {
        title: "Flaky".into(),
        ..NewIssue::default()
    };
    let specification = Specification::Issue(IssueSpecification {
        issue,
        repository: None,
        link: false,
        branch: "topic".into(),
        commit: "head".into(),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(
        &workspace,
        Target::IssueOpen,
        proposals::ISSUE,
        specification,
    );
    let (chosen, _) = manager
        .store_proposal(request, proposals::propose_issue)
        .await
        .unwrap();
    assert!(manager.pending_proposal(chosen.clone()).await.is_ok());

    // Another approval saves its edits before this decision takes the gate.
    let mut edited = chosen.clone();
    edited.edit(&ProposalDecision {
        title: Some("Flaky test".into()),
        ..ProposalDecision::default()
    });
    manager.save_proposal(edited).await.unwrap();
    let error = manager.pending_proposal(chosen).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("changed while it waited"),
        "{error:#}"
    );
}

#[tokio::test]
async fn a_duplicate_decision_refuses_a_number_reused_while_it_waited() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut requests = Vec::new();
    for name in ["original", "duplicate", "replacement"] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        requests.push(store_issue_proposal(&manager, &workspace).await);
    }
    let [original, duplicate, replacement] = &requests[..] else {
        unreachable!()
    };
    let decision = ProposalDecision {
        duplicate_of: Some(Duplicate::Approval(original.id)),
        ..ProposalDecision::default()
    };
    let locked = manager.duplicated_proposal(&decision).await.unwrap();
    assert_eq!(
        locked.as_ref().map(|locked| &locked.key),
        Some(&original.key)
    );

    // The original is decided and another workspace's proposal takes
    // its number while the duplicate decision waits for the gates.
    manager.forget_proposal(original.key.clone()).await.unwrap();
    manager
        .forget_proposal(replacement.key.clone())
        .await
        .unwrap();
    let workspace = manager.workspace(&replacement.workspace_id).await.unwrap();
    let reused = store_issue_proposal(&manager, &workspace).await;
    assert_eq!(reused.id, original.id);
    let error = manager
        .add_to_proposal(duplicate, original.id, locked.as_ref())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("changed while this decision waited"),
        "{error:#}"
    );
    let unchanged = manager.approval_request(reused.id).await.unwrap().unwrap();
    assert!(unchanged.proposed_issue().unwrap().duplicates.is_empty());
}

/// A pending issue proposal from `workspace`.
pub(crate) async fn store_issue_proposal(
    manager: &Manager,
    workspace: &Workspace,
) -> ApprovalRequest {
    let specification = Specification::Issue(IssueSpecification {
        issue: NewIssue {
            title: format!("Flaky in {}", workspace.name),
            ..NewIssue::default()
        },
        repository: None,
        link: false,
        branch: workspace.branch.clone(),
        commit: "head".into(),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(
        workspace,
        Target::IssueOpen,
        proposals::ISSUE,
        specification,
    );
    let (request, _) = manager
        .store_proposal(request, proposals::propose_issue)
        .await
        .unwrap();
    request
}

#[tokio::test]
async fn a_withdrawn_issue_proposal_goes_with_its_unread_notification() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let specification = Specification::Issue(IssueSpecification {
        issue: NewIssue {
            title: "Flaky".into(),
            ..NewIssue::default()
        },
        repository: None,
        link: false,
        branch: "topic".into(),
        commit: "head".into(),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(
        &workspace,
        Target::IssueOpen,
        proposals::ISSUE,
        specification,
    );
    let (proposed, _) = manager
        .store_proposal(request, proposals::propose_issue)
        .await
        .unwrap();
    manager.notify_approval(&workspace.name, &proposed).await;
    assert_eq!(manager.notifications(true, 50).await.unwrap().len(), 1);
    // An approval that failed to open it kept the user's edited title.
    let mut edited = proposed.clone();
    edited.edit(&ProposalDecision {
        title: Some("Flaky test".into()),
        ..ProposalDecision::default()
    });
    manager.save_proposal(edited).await.unwrap();

    let withdrawn = manager
        .withdraw_approval(&workspace.name, proposed.id)
        .await
        .unwrap();
    assert_eq!(withdrawn.status, DecisionStatus::Withdrawn);
    // It would outlive the workspace, so it goes as a decided one does.
    assert!(manager.approval_requests(None).await.unwrap().is_empty());
    assert!(manager.notifications(true, 50).await.unwrap().is_empty());
    assert!(manager.pending_proposal(proposed).await.is_err());
}
