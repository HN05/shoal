use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::approvals::proposals::ProposalExt;
use crate::daemon::{
    approvals::proposals::PR,
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
        push: None,
    });
    let request = proposal(&workspace.id, Target::PrOpen, PR, specification);
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
        branch: Some("topic".into()),
        commit: Some("head".into()),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(
        &workspace.id,
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
    assert!(manager.pending_proposal(chosen).await.is_err());
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
    for name in ["original", "duplicate"] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        requests.push(store_issue_proposal(&manager, &workspace.clone().into()).await);
    }
    let [original, duplicate] = &requests[..] else {
        unreachable!()
    };
    let decision = ProposalDecision {
        duplicate_of: Some(Duplicate::Approval(original.number)),
        ..ProposalDecision::default()
    };
    let locked = manager.duplicated_proposal(&decision).await.unwrap();
    assert_eq!(
        locked.as_ref().map(|locked| &locked.key),
        Some(&original.key)
    );

    // The original is decided and its workspace's next proposal takes its
    // number while the duplicate decision waits for the gates.
    manager.forget_proposal(original).await.unwrap();
    let workspace = manager.workspace_by_id(&original.space_id).await.unwrap();
    let reused = store_issue_proposal(&manager, &workspace.clone().into()).await;
    assert_eq!(reused.number, original.number);
    let added = manager
        .add_to_proposal(duplicate, original.number, locked.as_ref())
        .await;
    assert!(added.is_err());
    let unchanged = manager
        .approval_request(reused.number)
        .await
        .unwrap()
        .unwrap();
    assert!(unchanged.proposed_issue().unwrap().duplicates.is_empty());
}

#[tokio::test]
async fn a_proposal_keeps_its_number_and_reserves_its_removed_space() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let proposer = create("proposer").await.unwrap();
    let proposed = store_issue_proposal(&manager, &proposer.clone().into()).await;
    assert_eq!(proposed.number.space, proposer.number);
    manager
        .remove_workspace(
            &SpaceSelector::id(&proposer.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();

    // The user still decides it by the number it was announced with, shown
    // with the space it came from.
    let listed = manager.approval_requests(None).await.unwrap();
    let [kept] = &listed[..] else {
        panic!("{listed:?}")
    };
    assert_eq!((kept.number, kept.space_removed), (proposed.number, true));
    // No new space takes the number while it waits.
    let next = create("next").await.unwrap();
    assert_ne!(next.number, proposer.number);
    manager.forget_proposal(kept).await.unwrap();
    let freed = create("freed").await.unwrap();
    assert_eq!(freed.number, proposer.number);
}

/// A pending issue proposal from `workspace`.
pub(crate) async fn store_issue_proposal(manager: &Manager, space: &LinkSpace) -> ApprovalRequest {
    let specification = Specification::Issue(IssueSpecification {
        issue: NewIssue {
            title: format!("Flaky in {}", space.name()),
            ..NewIssue::default()
        },
        repository: None,
        link: false,
        branch: space.branch().map(str::to_owned),
        commit: space.workspace().map(|_| "head".into()),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let mut request = proposal(
        space.id(),
        Target::IssueOpen,
        proposals::ISSUE,
        specification,
    );
    request.repository_id = Some(space.repository_id().to_owned());
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
        branch: Some("topic".into()),
        commit: Some("head".into()),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(
        &workspace.id,
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
        .withdraw_approval(&SpaceSelector::from(&workspace.name), proposed.number)
        .await
        .unwrap();
    assert_eq!(withdrawn.status, DecisionStatus::Withdrawn);
    // It would outlive the workspace, so it goes as a decided one does.
    assert!(manager.approval_requests(None).await.unwrap().is_empty());
    assert!(manager.notifications(true, 50).await.unwrap().is_empty());
    assert!(manager.pending_proposal(proposed).await.is_err());
}

#[tokio::test]
async fn a_parent_workspace_denies_a_routed_proposal_or_passes_it_up() {
    use crate::{
        removal::{InspectionPolicy, UnsavedWork},
        state::AgentState,
    };
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    for id in [&parent.id, &child.id] {
        manager
            .set_agent_state(&SpaceSelector::id(id), AgentState::Working, None, None)
            .await
            .unwrap();
    }
    let specification = Specification::Issue(IssueSpecification {
        issue: NewIssue {
            title: "Flaky".into(),
            ..NewIssue::default()
        },
        repository: None,
        link: false,
        branch: Some("child".into()),
        commit: Some("head".into()),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(
        &child.id,
        Target::IssueOpen,
        proposals::ISSUE,
        specification,
    );
    let (proposed, _) = manager
        .store_proposal(request, proposals::propose_issue)
        .await
        .unwrap();
    manager.notify_approval(&child.name, &proposed).await;
    let routed = manager
        .approval_request(proposed.number)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(routed.decider.as_deref(), Some(parent.id.as_str()));

    // Opening it, or commenting it on another issue, writes to the forge.
    for (approve, details) in [
        (true, ProposalDecision::default()),
        (
            false,
            ProposalDecision {
                duplicate_of: Some(Duplicate::Issue("12".into())),
                ..ProposalDecision::default()
            },
        ),
    ] {
        let decided = manager
            .decide_routed_approval(&parent.id, proposed.number, approve, None, details)
            .await;
        assert!(decided.is_err());
    }
    let denied = manager
        .decide_routed_approval(
            &parent.id,
            proposed.number,
            false,
            None,
            ProposalDecision {
                reason: Some("Not a bug".into()),
                ..ProposalDecision::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(denied.status, DecisionStatus::Denied);
    assert!(manager.pending_proposal(proposed).await.is_err());

    // An issue proposal outlives its workspace and the one above that
    // decides it, which gives it back to the user as it goes.
    let specification = Specification::Issue(IssueSpecification {
        issue: NewIssue {
            title: "Slow".into(),
            ..NewIssue::default()
        },
        repository: None,
        link: false,
        branch: Some("child".into()),
        commit: Some("head".into()),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(
        &child.id,
        Target::IssueOpen,
        proposals::ISSUE,
        specification,
    );
    let (pending, _) = manager
        .store_proposal(request, proposals::propose_issue)
        .await
        .unwrap();
    manager.notify_approval(&child.name, &pending).await;
    for id in [&child.id, &parent.id] {
        manager
            .remove_workspace(
                &SpaceSelector::id(id),
                UnsavedWork::default(),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
    }
    let given_back = manager
        .approval_request(pending.number)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (given_back.status, given_back.decider),
        (DecisionStatus::Pending, None)
    );
    let asked = manager
        .notifications(false, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|n| n.kind == crate::daemon::notifications::NotificationKind::ApprovalRequested)
        .count();
    assert_eq!(asked, 1);
}
