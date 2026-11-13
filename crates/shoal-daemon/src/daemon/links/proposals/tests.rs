use super::*;
use crate::client::repository_ids::RepositoryId;
use crate::client::space_names::SpaceSelector;
use crate::daemon::{
    approvals::proposals::PR,
    test_support::{manager, repository},
};
use crate::forge::items::attach::Attachment;
use crate::model::Repository;
use std::sync::Arc;

/// A manager with a registered repository and one of its workspaces.
async fn with_workspace() -> (tempfile::TempDir, Arc<Manager>, Repository, Workspace) {
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
    (root, manager, repo, workspace)
}

/// The PR the workspace proposes with `body` and `attachments`, stored as
/// pending.
pub(crate) async fn store_pull_proposal(
    manager: &Manager,
    workspace: &Workspace,
    body: String,
    attachments: Vec<Attachment>,
) -> ApprovalRequest {
    let specification = Specification::Pull(PullSpecification {
        pull: NewPull {
            body,
            attachments,
            ..NewPull::default()
        },
        commit: "head".into(),
        agent: None,
        follow_up: None,
        push: None,
    });
    let request = proposal(&workspace.id, Target::PrOpen, PR, specification);
    let (request, _) = manager
        .store_proposal(request, proposals::propose_pull)
        .await
        .unwrap();
    request
}

/// Shoal's copy of a file `name` the workspace proposes with a body that
/// shows it, for the repository `repository`.
pub(crate) async fn kept_copy(
    manager: &Manager,
    workspace: &Workspace,
    repository: &RepositoryId,
    name: &str,
) -> (String, Attachment) {
    let file = workspace.path.join(name);
    std::fs::write(&file, name).unwrap();
    let mut body = format!("![shot]({})", file.display());
    let mut attachments = vec![Attachment {
        path: file,
        alt: None,
    }];
    let _files = manager
        .lock_proposal_files_of(repository, &attachments)
        .await
        .unwrap();
    manager
        .keep_attachments(&workspace.id, repository, &mut body, &mut attachments)
        .await
        .unwrap();
    (body, attachments.remove(0))
}

#[tokio::test]
async fn copies_go_to_the_repository_directory_reserved_first_when_missing() {
    let (_root, manager, repo, workspace) = with_workspace().await;
    let (body, copy) = kept_copy(&manager, &workspace, &repo.id, "shot.png").await;
    let directory = repo.workspaces_dir.clone().unwrap();
    assert!(
        copy.path.starts_with(directory.join(".proposals")),
        "{copy:?}"
    );
    assert_eq!(std::fs::read_to_string(&copy.path).unwrap(), "shot.png");
    assert!(body.contains(&copy.path.display().to_string()), "{body}");

    // A registration from before the repository directory layout reserves
    // its directory first.
    let id = repo.id.clone();
    manager
        .store
        .run(move |db| {
            Ok(db.execute(
                "UPDATE repositories SET workspaces_dir=NULL WHERE id=?1",
                [id],
            )?)
        })
        .await
        .unwrap();
    let (_, copy) = kept_copy(&manager, &workspace, &repo.id, "other.png").await;
    let reserved = manager.repository(&repo.id).await.unwrap().workspaces_dir;
    let reserved = reserved.expect("a reserved directory");
    assert!(
        copy.path.starts_with(reserved.join(".proposals")),
        "{copy:?}"
    );
    assert!(!manager.paths.state.join("proposals").exists());
}

#[tokio::test]
async fn the_sweep_keeps_only_files_pending_proposals_attach() {
    let (_root, manager, repo, workspace) = with_workspace().await;
    let (_, kept) = kept_copy(&manager, &workspace, &repo.id, "shot.png").await;
    let files = repo.workspaces_dir.clone().unwrap().join(".proposals");
    std::fs::create_dir_all(files.join("stale/0")).unwrap();
    store_pull_proposal(&manager, &workspace, String::new(), vec![kept.clone()]).await;

    manager.sweep_proposal_files().await.unwrap();
    assert!(kept.path.exists());
    assert!(!files.join("stale").exists());

    // Removing the workspace takes its PR proposal, and the next sweep
    // its files.
    manager
        .store
        .run(|db| Ok(db.execute("DELETE FROM workspaces", [])?))
        .await
        .unwrap();
    manager.sweep_proposal_files().await.unwrap();
    assert!(!kept.path.exists());
}

#[tokio::test]
async fn a_decided_proposal_takes_its_files_and_a_removed_repository_the_rest() {
    let (_root, manager, repo, workspace) = with_workspace().await;
    let (_, decided) = kept_copy(&manager, &workspace, &repo.id, "decided.png").await;
    let request =
        store_pull_proposal(&manager, &workspace, String::new(), vec![decided.clone()]).await;
    manager.forget_proposal(&request).await.unwrap();
    manager.prune_proposal_files(&request).await;
    assert!(!decided.path.exists());

    let (_, left) = kept_copy(&manager, &workspace, &repo.id, "left.png").await;
    manager.remove_repository(&repo.id).await.unwrap();
    assert!(!left.path.exists());
    let files = repo.workspaces_dir.clone().unwrap().join(".proposals");
    assert!(!files.exists());
}

#[tokio::test]
async fn copies_from_before_the_upgrade_stay_until_no_proposal_attaches_them() {
    let (_root, manager, _repo, workspace) = with_workspace().await;
    let old = manager.paths.state.join("proposals");
    let kept = old.join("kept/0/shot.png");
    std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
    std::fs::write(&kept, "png").unwrap();
    std::fs::create_dir_all(old.join("stale/0")).unwrap();
    let attachment = Attachment {
        path: kept.clone(),
        alt: None,
    };
    let request = store_pull_proposal(&manager, &workspace, String::new(), vec![attachment]).await;

    manager.sweep_proposal_files().await.unwrap();
    assert!(kept.exists());
    assert!(!old.join("stale").exists());

    manager.forget_proposal(&request).await.unwrap();
    manager.prune_proposal_files(&request).await;
    assert!(!old.exists());
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
        branch: space.branch().map(str::to_owned).map(BranchName::from),
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
    manager.notify_approval(&proposed).await;
    assert_eq!(manager.notifications(true, 50).await.unwrap().len(), 1);
    // An approval that failed to open it kept the user's edited title.
    let mut edited = proposed.clone();
    edited.edit(&ProposalDecision {
        title: Some("Flaky test".into()),
        ..ProposalDecision::default()
    });
    manager.save_proposal(edited).await.unwrap();

    let withdrawn = manager
        .withdraw_approval(&SpaceSelector::id(&workspace.id), proposed.number)
        .await
        .unwrap();
    assert_eq!(withdrawn.status, DecisionStatus::Withdrawn);
    // It would outlive the workspace, so it goes as a decided one does.
    assert!(manager.approval_requests(None).await.unwrap().is_empty());
    assert!(manager.notifications(true, 50).await.unwrap().is_empty());
    assert!(manager.pending_proposal(proposed).await.is_err());
}

#[tokio::test]
async fn a_parent_without_the_permission_denies_a_routed_proposal_or_passes_it_up() {
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
    let config = "[permissions]\nissue_open = 'propose'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
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
    manager.notify_approval(&proposed).await;
    let routed = manager
        .approval_request(proposed.number)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(routed.decider.as_ref(), Some(&parent.id));

    // The parent proposes its own issues, so it may not open this one, with
    // or without edits.
    for title in [None, Some("Flaky test".to_owned())] {
        let details = ProposalDecision {
            title,
            ..ProposalDecision::default()
        };
        let decided = manager
            .decide_routed_approval(&parent.id, proposed.number, true, None, details)
            .await;
        assert!(decided.is_err());
    }
    // It may deny it as a duplicate of another proposal, which takes its
    // finding, and deny that one with a reason.
    let original = store_issue_proposal(&manager, &child.clone().into()).await;
    manager.notify_approval(&original).await;
    let duplicate = ProposalDecision {
        duplicate_of: Some(Duplicate::Approval(original.number)),
        ..ProposalDecision::default()
    };
    let denied = manager
        .decide_routed_approval(&parent.id, proposed.number, false, None, duplicate)
        .await
        .unwrap();
    assert_eq!(denied.status, DecisionStatus::Denied);
    assert!(manager.pending_proposal(proposed).await.is_err());
    let original = manager
        .approval_request(original.number)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.findings().len(), 1);
    // A proposal outside its reach takes no finding of its.
    let outsider = manager
        .create_workspace(&repo.id, "outsider".into(), None, None, None)
        .await
        .unwrap();
    let outside = store_issue_proposal(&manager, &outsider.into()).await;
    let duplicate = ProposalDecision {
        duplicate_of: Some(Duplicate::Approval(outside.number)),
        ..ProposalDecision::default()
    };
    let decided = manager
        .decide_routed_approval(&parent.id, original.number, false, None, duplicate)
        .await;
    assert!(decided.is_err());
    let outside = manager.approval_request(outside.number).await.unwrap();
    assert!(outside.unwrap().findings().is_empty());
    let denied = manager
        .decide_routed_approval(
            &parent.id,
            original.number,
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

    // Marking one for another repository a duplicate of an issue there
    // comments on it, which the parent's issue_other_repositories withholds.
    let specification = Specification::Issue(IssueSpecification {
        issue: NewIssue {
            title: "Flaky upstream".into(),
            ..NewIssue::default()
        },
        repository: Some("https://github.com/other/project".into()),
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
    let (elsewhere, _) = manager
        .store_proposal(request, proposals::propose_issue)
        .await
        .unwrap();
    manager.notify_approval(&elsewhere).await;
    let duplicate = ProposalDecision {
        duplicate_of: Some(Duplicate::Issue("12".parse().unwrap())),
        ..ProposalDecision::default()
    };
    let decided = manager
        .decide_routed_approval(&parent.id, elsewhere.number, false, None, duplicate)
        .await;
    assert!(decided.is_err());
    let kept = manager.approval_request(elsewhere.number).await.unwrap();
    assert_eq!(kept.unwrap().status, DecisionStatus::Pending);
    let reason = ProposalDecision {
        reason: Some("Not ours".into()),
        ..ProposalDecision::default()
    };
    let denied = manager
        .decide_routed_approval(&parent.id, elsewhere.number, false, None, reason)
        .await
        .unwrap();
    assert_eq!(denied.status, DecisionStatus::Denied);

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
    manager.notify_approval(&pending).await;
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

#[tokio::test]
async fn approving_again_refuses_an_item_posted_before_the_origin_moved() {
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
    let mut request = store_issue_proposal(&manager, &LinkSpace::from(workspace)).await;
    request.url = Some("https://forge.example/team/old/issues/7".parse().unwrap());
    // The new origin's issue 7 is another issue, which nothing may edit.
    let moved = ForgeRepo::parse("https://forge.example/team/new").unwrap();
    let account = manager.user_account();
    let finished = manager
        .finish_proposed(&mut request, &moved, &account, ItemKind::Issue)
        .await
        .unwrap_err();
    assert!(
        format!("{finished:#}").contains("different repository"),
        "{finished:#}"
    );
}
