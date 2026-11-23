use super::*;
use crate::daemon::{
    approvals::proposals::PR,
    manager::Manager,
    scope::Caller,
    test_support::{manager, repository},
};
use crate::forge::items::attach::Attachment;
use crate::forge::items::create::NewPull;
use crate::forge::{ForgeRepo, items::link::ItemKind};
use crate::model::{Repository, Workspace};
use shoal_ids::repository_ids::RepositoryId;
use shoal_links::{daemon::ProposalKind, proposals::proposal};
use shoal_protocol::approvals::{
    ApprovalRequest, DecisionStatus, Duplicate, ProposalDecision, PullSpecification, Specification,
    Target,
};
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

/// A manager with a registered repository and one of its workspaces.
async fn with_workspace() -> (tempfile::TempDir, Arc<Manager>, Repository, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
        .links
        .store_proposal(request, ProposalKind::Pull)
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
    let _files = manager.gates.lock_proposal_files().await;
    manager
        .links
        .keep_attachments(
            manager,
            &workspace.id,
            repository,
            &mut body,
            &mut attachments,
        )
        .await
        .unwrap();
    (body, attachments.remove(0))
}

#[tokio::test]
async fn copies_go_to_the_repository_directory() {
    let (_root, manager, repo, workspace) = with_workspace().await;
    let (body, copy) = kept_copy(&manager, &workspace, &repo.id, "shot.png").await;
    let directory = repo.workspaces_dir.clone();
    assert!(
        copy.path.starts_with(directory.join(".proposals")),
        "{copy:?}"
    );
    assert_eq!(std::fs::read_to_string(&copy.path).unwrap(), "shot.png");
    assert!(body.contains(&copy.path.display().to_string()), "{body}");
    assert!(!manager.paths.state.join("proposals").exists());
}

#[tokio::test]
async fn the_sweep_keeps_only_files_pending_proposals_attach() {
    let (_root, manager, repo, workspace) = with_workspace().await;
    let (_, kept) = kept_copy(&manager, &workspace, &repo.id, "shot.png").await;
    let files = repo.workspaces_dir.clone().join(".proposals");
    std::fs::create_dir_all(files.join("stale/0")).unwrap();
    store_pull_proposal(&manager, &workspace, String::new(), vec![kept.clone()]).await;

    manager.links.sweep_proposal_files(&*manager).await.unwrap();
    assert!(kept.path.exists());
    assert!(!files.join("stale").exists());

    // Removing the workspace takes its PR proposal, and the next sweep
    // its files.
    manager
        .store
        .run(|db| Ok(db.execute("DELETE FROM spaces WHERE kind='workspace'", [])?))
        .await
        .unwrap();
    manager.links.sweep_proposal_files(&*manager).await.unwrap();
    assert!(!kept.path.exists());
}

#[tokio::test]
async fn a_decided_proposal_takes_its_files_and_a_removed_repository_the_rest() {
    let (_root, manager, repo, workspace) = with_workspace().await;
    let (_, decided) = kept_copy(&manager, &workspace, &repo.id, "decided.png").await;
    let request =
        store_pull_proposal(&manager, &workspace, String::new(), vec![decided.clone()]).await;
    manager.links.forget_proposal(&request).await.unwrap();
    manager
        .links
        .prune_proposal_files(&*manager, &request)
        .await;
    assert!(!decided.path.exists());

    let (_, left) = kept_copy(&manager, &workspace, &repo.id, "left.png").await;
    manager
        .repositories
        .remove(&*manager, &repo.id)
        .await
        .unwrap();
    assert!(!left.path.exists());
    let files = repo.workspaces_dir.clone().join(".proposals");
    assert!(!files.exists());
}

#[tokio::test]
async fn a_decision_refuses_a_proposal_changed_while_it_waited() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
        workspace: Some(ProposedAt {
            branch: Some("topic".into()),
            commit: "head".into(),
        }),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(&workspace.id, Target::IssueOpen, ISSUE, specification);
    let (chosen, _) = manager
        .links
        .store_proposal(request, ProposalKind::Issue)
        .await
        .unwrap();
    assert!(manager.links.pending_proposal(chosen.clone()).await.is_ok());

    // Another approval saves its edits before this decision takes the gate.
    let mut edited = chosen.clone();
    edited.edit(&ProposalDecision {
        title: Some("Flaky test".into()),
        ..ProposalDecision::default()
    });
    manager.links.save_proposal(edited).await.unwrap();
    assert!(manager.links.pending_proposal(chosen).await.is_err());
}

#[tokio::test]
async fn a_duplicate_decision_refuses_a_number_reused_while_it_waited() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
    let locked = manager.links.duplicated_proposal(&decision).await.unwrap();
    assert_eq!(
        locked.as_ref().map(|locked| &locked.key),
        Some(&original.key)
    );

    // The original is decided and its workspace's next proposal takes its
    // number while the duplicate decision waits for the gates.
    manager.links.forget_proposal(original).await.unwrap();
    let workspace = manager
        .spaces
        .workspace_by_id(&original.space_id)
        .await
        .unwrap();
    let reused = store_issue_proposal(&manager, &workspace.clone().into()).await;
    assert_eq!(reused.number, original.number);
    let added = manager
        .links
        .add_to_proposal(duplicate, original.number, locked.as_ref())
        .await;
    assert!(added.is_err());
    let unchanged = manager
        .links
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
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let proposer = create("proposer").await.unwrap();
    let proposed = store_issue_proposal(&manager, &proposer.clone().into()).await;
    assert_eq!(proposed.number.space, proposer.number);
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&proposer.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();

    // The user still decides it by the number it was announced with, shown
    // with the space it came from.
    let listed = manager
        .coordination
        .approval_requests(&*manager, None)
        .await
        .unwrap();
    let [kept] = &listed[..] else {
        panic!("{listed:?}")
    };
    assert_eq!((kept.number, kept.space_removed), (proposed.number, true));
    // No new space takes the number while it waits.
    let next = create("next").await.unwrap();
    assert_ne!(next.number, proposer.number);
    manager.links.forget_proposal(kept).await.unwrap();
    let freed = create("freed").await.unwrap();
    assert_eq!(freed.number, proposer.number);
}

#[tokio::test]
async fn a_withdrawn_issue_proposal_goes_with_its_unread_notification() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
        workspace: Some(ProposedAt {
            branch: Some("topic".into()),
            commit: "head".into(),
        }),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(&workspace.id, Target::IssueOpen, ISSUE, specification);
    let (proposed, _) = manager
        .links
        .store_proposal(request, ProposalKind::Issue)
        .await
        .unwrap();
    manager
        .coordination
        .notify_approval(&*manager, &proposed)
        .await;
    assert_eq!(manager.notifications.list(true, 50).await.unwrap().len(), 1);
    // An approval that failed to open it kept the user's edited title.
    let mut edited = proposed.clone();
    edited.edit(&ProposalDecision {
        title: Some("Flaky test".into()),
        ..ProposalDecision::default()
    });
    manager.links.save_proposal(edited).await.unwrap();

    let withdrawn = manager
        .coordination
        .withdraw_approval(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            proposed.number,
        )
        .await
        .unwrap();
    assert_eq!(withdrawn.status, DecisionStatus::Withdrawn);
    // It would outlive the workspace, so it goes as a decided one does.
    assert!(
        manager
            .coordination
            .approval_requests(&*manager, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        manager
            .notifications
            .list(true, 50)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(manager.links.pending_proposal(proposed).await.is_err());
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
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    for id in [&parent.id, &child.id] {
        manager
            .agents
            .set_agent_state(
                &*manager,
                &SpaceSelector::id(id),
                AgentState::Working,
                None,
                None,
            )
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
        workspace: Some(ProposedAt {
            branch: Some("child".into()),
            commit: "head".into(),
        }),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(&child.id, Target::IssueOpen, ISSUE, specification);
    let (proposed, _) = manager
        .links
        .store_proposal(request, ProposalKind::Issue)
        .await
        .unwrap();
    manager
        .coordination
        .notify_approval(&*manager, &proposed)
        .await;
    let routed = manager
        .links
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
            .coordination
            .decide_routed_approval(
                &*manager,
                &Caller::of_space(parent.id.clone()),
                proposed.number,
                true,
                None,
                details,
            )
            .await;
        assert!(decided.is_err());
    }
    // It may deny it as a duplicate of another proposal, which takes its
    // finding, and deny that one with a reason.
    let original = store_issue_proposal(&manager, &child.clone().into()).await;
    manager
        .coordination
        .notify_approval(&*manager, &original)
        .await;
    let duplicate = ProposalDecision {
        duplicate_of: Some(Duplicate::Approval(original.number)),
        ..ProposalDecision::default()
    };
    let denied = manager
        .coordination
        .decide_routed_approval(
            &*manager,
            &Caller::of_space(parent.id.clone()),
            proposed.number,
            false,
            None,
            duplicate,
        )
        .await
        .unwrap();
    assert_eq!(denied.status, DecisionStatus::Denied);
    assert!(manager.links.pending_proposal(proposed).await.is_err());
    let original = manager
        .links
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
        .coordination
        .decide_routed_approval(
            &*manager,
            &Caller::of_space(parent.id.clone()),
            original.number,
            false,
            None,
            duplicate,
        )
        .await;
    assert!(decided.is_err());
    let outside = manager
        .links
        .approval_request(outside.number)
        .await
        .unwrap();
    assert!(outside.unwrap().findings().is_empty());
    let denied = manager
        .coordination
        .decide_routed_approval(
            &*manager,
            &Caller::of_space(parent.id.clone()),
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
        workspace: Some(ProposedAt {
            branch: Some("child".into()),
            commit: "head".into(),
        }),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(&child.id, Target::IssueOpen, ISSUE, specification);
    let (elsewhere, _) = manager
        .links
        .store_proposal(request, ProposalKind::Issue)
        .await
        .unwrap();
    manager
        .coordination
        .notify_approval(&*manager, &elsewhere)
        .await;
    let duplicate = ProposalDecision {
        duplicate_of: Some(Duplicate::Issue("12".parse().unwrap())),
        ..ProposalDecision::default()
    };
    let decided = manager
        .coordination
        .decide_routed_approval(
            &*manager,
            &Caller::of_space(parent.id.clone()),
            elsewhere.number,
            false,
            None,
            duplicate,
        )
        .await;
    assert!(decided.is_err());
    let kept = manager
        .links
        .approval_request(elsewhere.number)
        .await
        .unwrap();
    assert_eq!(kept.unwrap().status, DecisionStatus::Pending);
    let reason = ProposalDecision {
        reason: Some("Not ours".into()),
        ..ProposalDecision::default()
    };
    let denied = manager
        .coordination
        .decide_routed_approval(
            &*manager,
            &Caller::of_space(parent.id.clone()),
            elsewhere.number,
            false,
            None,
            reason,
        )
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
        workspace: Some(ProposedAt {
            branch: Some("child".into()),
            commit: "head".into(),
        }),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let request = proposal(&child.id, Target::IssueOpen, ISSUE, specification);
    let (pending, _) = manager
        .links
        .store_proposal(request, ProposalKind::Issue)
        .await
        .unwrap();
    manager
        .coordination
        .notify_approval(&*manager, &pending)
        .await;
    for id in [&child.id, &parent.id] {
        manager
            .spaces
            .remove_selected(
                &*manager,
                &SpaceSelector::id(id),
                UnsavedWork::default(),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
    }
    let given_back = manager
        .links
        .approval_request(pending.number)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (given_back.status, given_back.decider),
        (DecisionStatus::Pending, None)
    );
    let asked = manager
        .notifications
        .list(false, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|n| n.kind == crate::daemon::notifications::AlertKind::ApprovalRequested)
        .count();
    assert_eq!(asked, 1);
}

#[tokio::test]
async fn approving_again_refuses_an_item_posted_before_the_origin_moved() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
    let account = manager.links.user_account();
    let finished = manager
        .links
        .finish_proposed(&mut request, &moved, &account, ItemKind::Issue)
        .await
        .unwrap_err();
    assert!(
        format!("{finished:#}").contains("different repository"),
        "{finished:#}"
    );
}
