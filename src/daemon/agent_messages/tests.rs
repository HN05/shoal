use super::Delivery;
use crate::{
    daemon::{
        scope::{self, Caller},
        workspace::{ExecutionKind, Manager},
        workspace_files::Attacher,
    },
    protocol::AgentMethod,
    test_support::{manager, repository},
};

async fn workspace(
    temp: &tempfile::TempDir,
    manager: &Manager,
    name: &str,
) -> crate::model::Workspace {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, name.into(), None, None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn messages_wait_until_delivered_and_repeats_collapse() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let changed = manager.signals.watch_agent_messages();
    for _ in 0..2 {
        manager
            .send_agent_message(
                &workspace.name,
                "Stop the dev server".into(),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    assert!(changed.has_changed().unwrap());
    manager
        .send_agent_message(
            &workspace.name,
            "Rebase onto main".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .send_agent_message(
            &workspace.name,
            "Move:\n\tabc123 to api".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    for rejected in [
        "\x1b]9;spoof\x07".to_owned(),
        "x".repeat(crate::validate::MAX_AGENT_MESSAGE_LEN + 1),
    ] {
        assert!(
            manager
                .send_agent_message(&workspace.name, rejected, Vec::new(), &Attacher::User)
                .await
                .is_err()
        );
    }
    let messages = manager.agent_messages(&workspace.name).await.unwrap();
    let texts: Vec<_> = messages.iter().map(|m| m.message.as_str()).collect();
    assert_eq!(
        texts,
        [
            "Stop the dev server",
            "Rebase onto main",
            "Move:\n\tabc123 to api"
        ]
    );
    manager
        .mark_agent_messages_delivered(&workspace.name, vec![messages[0].id])
        .await
        .unwrap();
    let remaining = manager.agent_messages(&workspace.name).await.unwrap();
    assert_eq!(remaining, messages[1..]);
    // A delivered message may be sent again.
    manager
        .send_agent_message(
            &workspace.name,
            "Stop the dev server".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(
        manager.agent_messages(&workspace.name).await.unwrap().len(),
        3
    );
}

#[tokio::test]
async fn a_message_for_running_work_wakes_only_while_work_runs() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let queue = |delivery| {
        manager.queue_agent_message_delivered(&workspace.id, "Memory is low".into(), delivery)
    };
    let wakes = || async {
        manager
            .unread_agent_messages(&workspace.id)
            .await
            .unwrap()
            .wakes
    };
    queue(Delivery::WhileWorkRuns).await.unwrap();
    assert_eq!(
        manager.agent_messages(&workspace.id).await.unwrap().len(),
        1
    );
    assert!(!wakes().await);
    // Queued again, it has waits check again for work.
    let mut changed = manager.signals.watch_agent_messages();
    changed.borrow_and_update();
    queue(Delivery::WhileWorkRuns).await.unwrap();
    assert!(changed.has_changed().unwrap());
    let command = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    assert!(wakes().await);
    manager
        .finish_execution(command.plan.id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    assert!(!wakes().await);
    // Queued to wake, it wakes without work and stays so.
    changed.borrow_and_update();
    queue(Delivery::Wake).await.unwrap();
    assert!(changed.has_changed().unwrap());
    assert!(wakes().await);
    queue(Delivery::WhileWorkRuns).await.unwrap();
    assert!(wakes().await);
    assert_eq!(
        manager.agent_messages(&workspace.id).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn unread_messages_are_bounded() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    for n in 0..super::MAX_UNREAD {
        manager
            .send_agent_message(
                &workspace.name,
                format!("message {n}"),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    assert!(
        manager
            .send_agent_message(
                &workspace.name,
                "one more".into(),
                Vec::new(),
                &Attacher::User
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn workspace_processes_read_their_own_messages_but_cannot_send() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let other = manager
        .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
        .await
        .unwrap();
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: workspace.id.clone(),
                execution: None,
            },
        )
        .await;
    for (target, allowed) in [(&workspace.name, true), (&other.name, false)] {
        for mut method in [
            AgentMethod::AgentMessages {
                workspace: target.clone(),
            },
            AgentMethod::MarkAgentMessagesDelivered {
                workspace: target.clone(),
                ids: Vec::new(),
            },
        ] {
            assert_eq!(
                scope::authorize(&manager, Some("scope"), &mut method)
                    .await
                    .is_ok(),
                allowed
            );
        }
    }
    let mut method = AgentMethod::SendAgentMessage {
        workspace: workspace.name.clone(),
        message: "hello".into(),
        attachments: Vec::new(),
        sender_chat: None,
    };
    assert!(
        scope::authorize(&manager, Some("scope"), &mut method)
            .await
            .is_err()
    );
}

/// Start a tracked agent in `workspace`; it runs while the returned
/// execution lives.
async fn run_agent(
    manager: &Manager,
    workspace: &crate::model::Workspace,
) -> crate::daemon::workspace::StartedExecution {
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    manager
        .track_agent(
            &execution.plan.id,
            &crate::execution::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            false,
            std::time::Duration::ZERO,
        )
        .await;
    execution
}

#[tokio::test]
async fn a_repository_message_reaches_its_running_agents_below_the_sender() {
    let (temp, manager) = manager().await;
    let parent = workspace(&temp, &manager, "parent").await;
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    let sibling = manager
        .create_workspace(&parent.repository_id, "sibling".into(), None, None, None)
        .await
        .unwrap();
    let idle = manager
        .create_workspace(&parent.repository_id, "idle".into(), None, None, None)
        .await
        .unwrap();
    let elsewhere = repository(temp.path(), "elsewhere");
    let elsewhere = manager
        .register_repository(elsewhere.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let outside = manager
        .create_workspace(&elsewhere.id, "outside".into(), None, None, None)
        .await
        .unwrap();
    let mut running = Vec::new();
    for workspace in [&parent, &child, &sibling, &outside] {
        running.push(run_agent(&manager, workspace).await);
    }
    let send = |below: Option<String>| {
        let manager = &manager;
        let repository = parent.repository_id.clone();
        async move {
            let reached = manager
                .send_repository_agent_message(
                    &repository.into(),
                    "Rebase onto main".into(),
                    Vec::new(),
                    &Attacher::User,
                    below.as_deref(),
                )
                .await
                .unwrap();
            reached.into_iter().map(|w| w.name).collect::<Vec<_>>()
        }
    };
    assert_eq!(send(None).await, ["child", "parent", "sibling"]);
    for workspace in [&parent, &child, &sibling] {
        let messages = manager.agent_messages(&workspace.id).await.unwrap();
        assert_eq!(messages.len(), 1, "{}", workspace.name);
    }
    for workspace in [&idle, &outside] {
        assert!(
            manager
                .agent_messages(&workspace.id)
                .await
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(send(Some(parent.id.clone())).await, ["child"]);
    assert!(send(Some(child.id.clone())).await.is_empty());
    // A workspace process names only its own repository.
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: parent.id.clone(),
                execution: None,
            },
        )
        .await;
    for (repository, allowed) in [(&parent.repository_id, true), (&elsewhere.id, false)] {
        let mut method = AgentMethod::SendRepositoryAgentMessage {
            repository: repository.clone().into(),
            message: "Rebase onto main".into(),
            attachments: Vec::new(),
            sender_chat: None,
        };
        assert_eq!(
            scope::authorize(&manager, Some("scope"), &mut method)
                .await
                .is_ok(),
            allowed
        );
    }
}

#[tokio::test]
async fn attached_files_are_copied_beside_the_workspaces_until_removal() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    for _ in 0..2 {
        manager
            .send_agent_message(
                &workspace.name,
                "See the log".into(),
                vec![file.clone().into()],
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    manager
        .send_agent_message(
            &workspace.name,
            String::new(),
            vec![file.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    std::fs::write(&file, "changed").unwrap();
    let messages = manager.agent_messages(&workspace.name).await.unwrap();
    let [attached, alone] = messages.as_slice() else {
        panic!("{messages:?}");
    };
    assert_eq!(alone.message, "");
    let [copy] = attached.attachments.as_slice() else {
        panic!("{attached:?}");
    };
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "panic");
    assert_eq!(copy.file_name(), file.file_name());
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    let workspaces_dir = repo.workspaces_dir.as_deref().unwrap();
    assert!(copy.starts_with(workspaces_dir.join(".attachments").join(&workspace.id)));

    manager
        .mark_agent_messages_delivered(&workspace.name, vec![attached.id])
        .await
        .unwrap();
    assert!(copy.exists(), "a delivered message keeps its files");
    manager
        .remove_workspace(
            &workspace.id,
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!copy.exists());
}

#[tokio::test]
async fn a_refused_message_leaves_no_copies() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    for n in 0..super::MAX_UNREAD {
        manager
            .send_agent_message(
                &workspace.name,
                format!("message {n}"),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    assert!(
        manager
            .send_agent_message(
                &workspace.name,
                "See the log".into(),
                vec![file.into()],
                &Attacher::User
            )
            .await
            .is_err()
    );
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    let attachments = repo.workspaces_dir.unwrap().join(".attachments");
    assert!(!attachments.join(&workspace.id).exists());
}

#[tokio::test]
async fn an_attached_send_waits_for_the_files_gate() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    // Another send, a removal or a move holds the gate.
    let held = manager
        .gates
        .lock_message_files(&workspace.repository_id)
        .await;
    let gate = manager.gates.message_files_gate(&workspace.repository_id);
    let send = tokio::spawn({
        let (manager, name) = (manager.clone(), workspace.name.clone());
        async move {
            manager
                .send_agent_message(
                    &name,
                    "See the log".into(),
                    vec![file.into()],
                    &Attacher::User,
                )
                .await
        }
    });
    // The holder, this test and the waiting send.
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while std::sync::Arc::strong_count(&gate) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the send reaches the gate");
    assert!(
        manager
            .agent_messages(&workspace.name)
            .await
            .unwrap()
            .is_empty()
    );
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    let attachments = repo.workspaces_dir.unwrap().join(".attachments");
    assert!(!attachments.join(&workspace.id).exists());
    drop(held);
    send.await.unwrap().unwrap();
    let messages = manager.agent_messages(&workspace.name).await.unwrap();
    assert!(messages[0].attachments[0].exists());
}

#[tokio::test]
async fn a_send_waiting_through_removal_leaves_no_files() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    let held = manager
        .gates
        .lock_message_files(&workspace.repository_id)
        .await;
    let gate = manager.gates.message_files_gate(&workspace.repository_id);
    let send = tokio::spawn({
        let (manager, id) = (manager.clone(), workspace.id.clone());
        async move {
            manager
                .send_agent_message(
                    &id,
                    "See the log".into(),
                    vec![file.into()],
                    &Attacher::User,
                )
                .await
        }
    });
    let removal = tokio::spawn({
        let (manager, id) = (manager.clone(), workspace.id.clone());
        async move {
            manager
                .remove_workspace(
                    &id,
                    crate::removal::UnsavedWork::default(),
                    crate::removal::InspectionPolicy::GitOnly,
                )
                .await
        }
    });
    // The holder, this test, the send and the removal deleting files.
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while std::sync::Arc::strong_count(&gate) < 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the send and the removal reach the gate");
    drop(held);
    assert!(removal.await.unwrap().unwrap().removed);
    assert!(send.await.unwrap().is_err());
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    let attachments = repo.workspaces_dir.unwrap().join(".attachments");
    assert!(!attachments.join(&workspace.id).exists());
}

#[tokio::test]
async fn attachments_must_be_files() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    for refused in [
        temp.path().to_path_buf(),
        temp.path().join("missing.png"),
        "relative.png".into(),
    ] {
        let error = manager
            .send_agent_message(
                &workspace.name,
                "See".into(),
                vec![refused.clone().into()],
                &Attacher::User,
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains(&*refused.to_string_lossy()),
            "{error:#}"
        );
    }
    assert!(
        manager
            .send_agent_message(&workspace.name, String::new(), Vec::new(), &Attacher::User)
            .await
            .is_err()
    );
    assert!(
        manager
            .agent_messages(&workspace.name)
            .await
            .unwrap()
            .is_empty()
    );
}
