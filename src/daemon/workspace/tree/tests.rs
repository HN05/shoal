use crate::{
    daemon::{
        events::{EventCause, EventItem, EventKind},
        scope::{Caller, ScopedExecution},
    },
    model::{CleanupReason, Workspace, WorkspaceRef},
    protocol::Method,
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{manager, repository},
};

fn reference(workspace: &Workspace) -> WorkspaceRef {
    WorkspaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        branch: workspace.branch.clone(),
    }
}

#[tokio::test]
async fn splitting_records_the_parent_and_removal_moves_children_up() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let root = manager
        .create_workspace(&repo.id, "root".into(), None, None, None)
        .await
        .unwrap();
    let middle = manager
        .split_workspace(&root.name, "middle".into(), None)
        .await
        .unwrap();
    // A base naming the parent's branch stacks the child on it too.
    let leaf = manager
        .split_workspace(&middle.id, "leaf".into(), Some("middle".into()))
        .await
        .unwrap();
    assert_eq!(middle.parent_workspace, Some(reference(&root)));
    assert_eq!(leaf.parent_workspace, Some(reference(&middle)));
    assert_eq!(leaf.base_workspace, Some(reference(&middle)));
    assert_eq!(
        manager.workspace("root").await.unwrap().child_workspaces,
        [reference(&middle)]
    );
    assert_eq!(
        manager.subtree(&root.id).await.unwrap(),
        [root.id.clone(), middle.id.clone(), leaf.id.clone()]
    );
    assert!(manager.within_subtree(&root.id, &leaf.id).await.unwrap());
    assert!(!manager.within_subtree(&leaf.id, &root.id).await.unwrap());

    for (removed, parent) in [(&middle, Some(reference(&root))), (&root, None)] {
        manager
            .remove_workspace(
                &removed.id,
                UnsavedWork::default(),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
        assert_eq!(
            manager.workspace("leaf").await.unwrap().parent_workspace,
            parent
        );
    }

    let events = manager.workspace_events(None, 100).await.unwrap();
    let parents: Vec<_> = events
        .iter()
        .filter_map(|item| match item {
            EventItem::Event(event)
                if event.details.workspace_id == leaf.id
                    && matches!(
                        event.details.kind,
                        EventKind::Created | EventKind::ParentChanged
                    ) =>
            {
                Some((
                    event.details.kind,
                    event.details.cause,
                    event.details.parent_workspace.clone(),
                ))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        parents,
        [
            (EventKind::Created, None, Some(Some(reference(&middle)))),
            (
                EventKind::ParentChanged,
                Some(EventCause::Removed),
                Some(Some(reference(&root)))
            ),
            (
                EventKind::ParentChanged,
                Some(EventCause::Removed),
                Some(None)
            ),
        ]
    );
}

#[tokio::test]
async fn workspace_processes_reach_their_subtree_and_split_within_it() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let parent = create("parent").await.unwrap();
    let sibling = create("sibling").await.unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    manager
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: child.id.clone(),
                execution: Some(ScopedExecution {
                    id: "execution".into(),
                    kind: super::super::ExecutionKind::Command,
                }),
            },
        )
        .await;
    let split = |workspace: &str| Method::SplitWorkspace {
        workspace: workspace.into(),
        name: "grandchild".into(),
        base: None,
    };
    for (target, allowed) in [
        (&child.name, true),
        (&parent.name, false),
        (&sibling.name, false),
    ] {
        let authorized =
            crate::daemon::scope::authorize(&manager, Some("scope"), &mut split(target)).await;
        assert_eq!(authorized.is_ok(), allowed, "{target}");
    }
    let grandchild = manager
        .split_workspace(&child.id, "grandchild".into(), None)
        .await
        .unwrap();
    let mut done = Method::WorkspaceDone {
        workspace: grandchild.name.clone(),
        cleanup: false,
    };
    crate::daemon::scope::authorize(&manager, Some("scope"), &mut done)
        .await
        .unwrap();
    // Targets resolve to the named workspace, not the caller's own.
    assert!(
        matches!(&done, Method::WorkspaceDone { workspace, .. } if *workspace == grandchild.id)
    );
}

#[tokio::test]
async fn children_keep_their_parent_from_cleanup() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    assert_eq!(
        manager.cleanup_reason(&parent).await,
        CleanupReason::Children
    );
    assert_eq!(
        manager.cleanup_reason(&child).await,
        CleanupReason::Idle { minutes: 10 }
    );
    manager
        .remove_workspace(&child.id, UnsavedWork::default(), InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert_eq!(
        manager.cleanup_reason(&parent).await,
        CleanupReason::Idle { minutes: 10 }
    );
}

#[tokio::test]
async fn a_parent_must_be_a_workspace_of_the_same_repository() {
    let (temp, manager) = manager().await;
    let mut repos = Vec::new();
    for name in ["repo", "other"] {
        let checkout = repository(temp.path(), name);
        repos.push(
            manager
                .register_repository(checkout.to_str().unwrap().into(), None, None)
                .await
                .unwrap(),
        );
    }
    let foreign = manager
        .create_workspace(&repos[1].id, "foreign".into(), None, None, None)
        .await
        .unwrap();
    let error = manager
        .store
        .run(move |db| super::ensure_parent(db, &repos[0].id, &foreign.id))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("not a workspace of this repository"));
}

#[tokio::test]
async fn a_parent_reserved_for_removal_refuses_splits() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    manager
        .reserve_lifecycle(&parent.id, crate::state::WorkspaceState::Removing)
        .await
        .unwrap();
    let error = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("parent workspace is removing"),
        "{error:#}"
    );
    assert!(manager.workspace("child").await.is_err());
    assert!(
        manager
            .workspace(&parent.id)
            .await
            .unwrap()
            .child_workspaces
            .is_empty()
    );
}

#[tokio::test]
async fn scoped_lookups_find_items_and_leases_in_the_subtree() {
    let (temp, manager) = manager().await;
    crate::test_support::configure(&manager, "[resources.lock]\ncapacity = 2\n").await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let sibling = manager
        .create_workspace(&repo.id, "sibling".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    let issue = "https://github.com/owner/repo/issues/7";
    for workspace in [&child, &sibling] {
        let id = workspace.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO workspace_items(workspace_id,relation,kind,url)
                     VALUES (?1,'related','issue',?2)",
                    [&id, issue],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        manager
            .acquire_resource(
                &workspace.id,
                crate::daemon::resources::ResourceRequest {
                    mode: None,
                    kind: None,
                    pool: "lock".into(),
                    name: workspace.name.clone(),
                    resource: None,
                    reason: None,
                    queue: false,
                },
                None,
            )
            .await
            .unwrap();
    }
    let targets = [
        crate::model::WorkspaceTarget::Item {
            kind: crate::forge::link::ItemKind::Issue,
            input: "7".into(),
        },
        crate::model::WorkspaceTarget::Resource {
            name: "lock".into(),
        },
    ];
    let subtree = manager.subtree(&parent.id).await.unwrap();
    for target in &targets {
        let found = manager
            .find_workspaces(target, Some(subtree.clone()))
            .await
            .unwrap();
        let names: Vec<_> = found.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["child"], "{target:?}");
        let everywhere = manager.find_workspaces(target, None).await.unwrap();
        assert_eq!(everywhere.len(), 2, "{target:?}");
    }
}

#[tokio::test]
async fn coordinating_agents_receive_their_childrens_events_instead_of_the_user() {
    use crate::{daemon::notifications::NotificationKind, state::AgentState};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    let helper = manager
        .split_workspace(&parent.id, "helper".into(), None)
        .await
        .unwrap();
    let user = || async {
        manager
            .notifications(false, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.message)
            .collect::<Vec<_>>()
    };
    let received = || async {
        manager
            .agent_messages(&parent.id)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.message)
            .collect::<Vec<_>>()
    };
    manager
        .set_agent_state(&child.id, AgentState::Working, None, None)
        .await
        .unwrap();
    // Without an agent above, the user hears the child's agent.
    manager
        .send_message(&child.id, "first".into())
        .await
        .unwrap();
    assert_eq!(user().await, ["first"]);

    manager
        .set_agent_state(&parent.id, AgentState::Working, None, None)
        .await
        .unwrap();
    manager
        .send_message(&child.id, "second".into())
        .await
        .unwrap();
    // A child without an agent of its own is the coordinator's own work.
    manager
        .send_message(&helper.id, "from the parent".into())
        .await
        .unwrap();
    // The daemon's events about a child's agent reach the coordinator.
    manager
        .notify(
            Some(&helper.name),
            NotificationKind::AgentExited,
            "claude exited",
        )
        .await;
    assert_eq!(received().await, ["child: second", "helper: claude exited"]);
    assert_eq!(user().await, ["first", "from the parent"]);
}

#[tokio::test]
async fn coordinating_agents_answer_and_message_the_workspaces_below_them() {
    use crate::{daemon::questions::Ask, state::AgentState};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let parent = create("parent").await.unwrap();
    let sibling = create("sibling").await.unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    for workspace in [&parent, &child] {
        manager
            .set_agent_state(&workspace.id, AgentState::Working, None, None)
            .await
            .unwrap();
    }
    let ask = |question: &str| Ask {
        question: question.into(),
        recommendation: "Yes".into(),
        reason: None,
        choices: vec![],
    };
    let below = manager
        .ask_question(&child.id, ask("Use SQLite?"))
        .await
        .unwrap();
    let own = manager
        .ask_question(&parent.id, ask("Merge now?"))
        .await
        .unwrap();
    let beside = manager
        .ask_question(&sibling.id, ask("Rename it?"))
        .await
        .unwrap();
    let messages = manager.agent_messages(&parent.id).await.unwrap();
    assert!(
        messages[0]
            .message
            .starts_with(&format!("child: question {}", below.id)),
        "{messages:?}"
    );
    manager
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: parent.id.clone(),
                execution: None,
            },
        )
        .await;
    for (workspace, allowed) in [(&child, true), (&parent, false), (&sibling, false)] {
        let mut message = Method::SendAgentMessage {
            workspace: workspace.name.clone(),
            message: "Rebase onto main".into(),
        };
        let authorized =
            crate::daemon::scope::authorize(&manager, Some("scope"), &mut message).await;
        assert_eq!(authorized.is_ok(), allowed, "{}", workspace.name);
    }
    for (question, allowed) in [(&below, true), (&own, false), (&beside, false)] {
        let mut answer = Method::AnswerQuestion {
            id: question.id,
            answer: "Yes".into(),
            shown: None,
        };
        let authorized =
            crate::daemon::scope::authorize(&manager, Some("scope"), &mut answer).await;
        assert_eq!(authorized.is_ok(), allowed, "{}", question.ask.question);
    }
}

#[tokio::test]
async fn tracked_agents_without_turn_states_coordinate_and_report() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    // Custom and Happy agents report no turn state; the daemon tracks them.
    let mut connections = Vec::new();
    for workspace in [&parent, &child] {
        let started = manager
            .begin_execution(
                &workspace.id,
                None,
                super::super::ExecutionKind::Command,
                None,
            )
            .await
            .unwrap();
        manager
            .track_agent(
                &started.plan.id,
                "droid",
                &workspace.id,
                &workspace.name,
                false,
                std::time::Duration::ZERO,
            )
            .await;
        connections.push(started.stop);
    }
    manager
        .send_message(&child.id, "done with the API".into())
        .await
        .unwrap();
    let received = manager.agent_messages(&parent.id).await.unwrap();
    assert_eq!(received[0].message, "child: done with the API");
    assert!(manager.notifications(false, 10).await.unwrap().is_empty());
    // The child's own agent receives its activity instead of the parent's.
    assert!(
        manager
            .delegated_workspaces(&parent.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_scoped_answer_binds_to_the_question_it_was_authorized_for() {
    use crate::daemon::questions::Ask;
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let parent = create("parent").await.unwrap();
    let sibling = create("sibling").await.unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    let ask = |question: &str| Ask {
        question: question.into(),
        recommendation: "Yes".into(),
        reason: None,
        choices: vec![],
    };
    let asked = manager
        .ask_question(&child.id, ask("Use SQLite?"))
        .await
        .unwrap();
    manager
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: parent.id.clone(),
                execution: None,
            },
        )
        .await;
    let mut answer = Method::AnswerQuestion {
        id: asked.id,
        answer: "Yes".into(),
        shown: None,
    };
    crate::daemon::scope::authorize(&manager, Some("scope"), &mut answer)
        .await
        .unwrap();
    let Method::AnswerQuestion { id, answer, shown } = answer else {
        panic!("authorization keeps the method");
    };
    assert!(shown.is_some());
    // Meanwhile the question is answered and delivered, and the sibling's
    // next question reuses its number.
    let answered = manager
        .answer_question(asked.id, "No".into(), None)
        .await
        .unwrap();
    manager
        .mark_agent_messages_delivered(&child.id, vec![answered.message_id.unwrap()])
        .await
        .unwrap();
    let reused = manager
        .ask_question(&sibling.id, ask("Rename it?"))
        .await
        .unwrap();
    assert_eq!(reused.id, asked.id);
    let error = manager
        .answer_question(id, answer, shown)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("changed since it was shown"));
    assert!(manager.question(reused.id).await.unwrap().answer.is_none());
}

#[tokio::test]
async fn a_launch_reopens_only_its_recorded_workspace() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    let opened = manager.reopen_workspace(&child.id).await.unwrap();
    assert!(opened.reused && opened.workspace.id == child.id);
    // Removed during the handoff, the child is not created again from its branch.
    manager
        .remove_workspace(&child.id, UnsavedWork::default(), InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert!(manager.reopen_workspace(&child.id).await.is_err());
    assert_eq!(manager.list_workspaces().await.unwrap().len(), 1);
}

#[tokio::test]
async fn approval_requests_go_to_the_nearest_coordinator_holding_a_grant() {
    use crate::{
        daemon::approvals::{
            ApprovalRequest, DecisionStatus, Lifetime, SecretSpecification, Specification, Target,
        },
        state::AgentState,
    };
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let root = manager
        .create_workspace(&repo.id, "root".into(), None, None, None)
        .await
        .unwrap();
    let parent = manager
        .split_workspace(&root.id, "parent".into(), None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    for workspace in [&root, &parent, &child] {
        manager
            .set_agent_state(&workspace.id, AgentState::Working, None, None)
            .await
            .unwrap();
    }
    let secret = |workspace: &Workspace| {
        ApprovalRequest::new(
            &workspace.id,
            Target::Secret("token".into()),
            "default",
            Specification::Secret(SecretSpecification {
                command: vec!["token-source".into()],
                tools: vec![],
            }),
            Lifetime::Workspace,
            Some("Publish the package"),
        )
    };
    // The root holds a grant for the secret; the parent does not.
    let granted = manager
        .request_approval(secret(&root))
        .await
        .unwrap()
        .unwrap();
    manager
        .decide_approval(granted.id, true, None)
        .await
        .unwrap();
    let requested = manager
        .request_approval(secret(&child))
        .await
        .unwrap()
        .unwrap();
    manager.notify_approval(&child.name, &requested).await;
    let routed = manager
        .approval_request(requested.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(routed.decider.as_deref(), Some(root.id.as_str()));
    let messages = |workspace: &Workspace| {
        let id = workspace.id.clone();
        let manager = &manager;
        async move {
            manager
                .agent_messages(&id)
                .await
                .unwrap()
                .into_iter()
                .map(|message| message.message)
                .collect::<Vec<_>>()
        }
    };
    assert!(messages(&parent).await[0].ends_with("passed up, since you hold no grant for it"));
    assert!(
        messages(&root)
            .await
            .last()
            .unwrap()
            .contains("shoal escalate")
    );
    let user = || async {
        manager
            .notifications(false, 10)
            .await
            .unwrap()
            .into_iter()
            .filter(|n| n.kind == crate::daemon::notifications::NotificationKind::ApprovalRequested)
            .count()
    };
    assert_eq!(user().await, 0);

    // Only the decider decides it, bound to the request it was shown.
    for (workspace, allowed) in [(&parent, false), (&root, true)] {
        manager
            .issue_scope(
                format!("scope-{}", workspace.name),
                Caller {
                    workspace_id: workspace.id.clone(),
                    execution: None,
                },
            )
            .await;
        let mut decide = Method::DecideApproval {
            id: requested.id,
            approve: true,
            shown: None,
        };
        let authorized = crate::daemon::scope::authorize(
            &manager,
            Some(&format!("scope-{}", workspace.name)),
            &mut decide,
        )
        .await;
        assert_eq!(authorized.is_ok(), allowed, "{}", workspace.name);
        if allowed {
            assert!(matches!(
                decide,
                Method::DecideApproval { shown: Some(_), .. }
            ));
        }
    }

    // Passed up by the root, it reaches the user; repeating the request
    // does not route it again.
    let escalated = manager
        .escalate_approval(&root.id, requested.id, None)
        .await
        .unwrap();
    assert_eq!(escalated.decider, None);
    assert_eq!(escalated.status, DecisionStatus::Pending);
    assert_eq!(user().await, 1);
    let error = manager
        .escalate_approval(&root.id, requested.id, None)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("not root's to decide"));
    manager.notify_approval(&child.name, &escalated).await;
    let again = manager
        .approval_request(requested.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.decider, None);
}
