use super::Delivery;
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::execution::recovery::LastSessionExt;
use crate::paths::StateFiles;
use crate::{
    daemon::{
        scope::{self, Caller},
        test_support::{manager, names, repository},
        workspace::{ExecutionKind, Manager},
        workspace_files::Attacher,
    },
    protocol::AgentMethod,
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
                &SpaceSelector::id(&workspace.id),
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
            &SpaceSelector::id(&workspace.id),
            "Rebase onto main".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .send_agent_message(
            &SpaceSelector::id(&workspace.id),
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
                .send_agent_message(
                    &SpaceSelector::id(&workspace.id),
                    rejected,
                    Vec::new(),
                    &Attacher::User
                )
                .await
                .is_err()
        );
    }
    let messages = manager
        .agent_messages(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
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
        .mark_agent_messages_delivered(&SpaceSelector::id(&workspace.id), vec![messages[0].id])
        .await
        .unwrap();
    let remaining = manager
        .agent_messages(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(remaining, messages[1..]);
    // A delivered message may be sent again.
    manager
        .send_agent_message(
            &SpaceSelector::id(&workspace.id),
            "Stop the dev server".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(
        manager
            .agent_messages(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn a_message_for_the_next_turn_wakes_only_once_queued_to_wake() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let queue = |delivery| {
        manager.queue_agent_message_delivered(&workspace.id, "Memory is high".into(), delivery)
    };
    let wakes = || async {
        manager
            .unread_agent_messages(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .wakes
    };
    // Work running in the workspace does not make it wake.
    let _command = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    queue(Delivery::NextTurn).await.unwrap();
    assert!(!wakes().await);
    let mut changed = manager.signals.watch_agent_messages();
    changed.borrow_and_update();
    queue(Delivery::NextTurn).await.unwrap();
    assert!(!changed.has_changed().unwrap());
    // Queued to wake, it wakes and stays so.
    queue(Delivery::Wake).await.unwrap();
    assert!(changed.has_changed().unwrap());
    assert!(wakes().await);
    queue(Delivery::NextTurn).await.unwrap();
    assert!(wakes().await);
    assert_eq!(
        manager
            .agent_messages(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .len(),
        1
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
                space_id: workspace.id.clone(),
                execution: None,
            },
        )
        .await;
    for (target, allowed) in [(&workspace.id, true), (&other.id, false)] {
        for mut method in [
            AgentMethod::AgentMessages {
                space: SpaceSelector::from(target),
            },
            AgentMethod::MarkAgentMessagesDelivered {
                space: SpaceSelector::from(target),
                ids: Vec::new(),
            },
        ] {
            assert_eq!(
                scope::authorize(&manager, Some(&"scope".into()), &mut method)
                    .await
                    .is_ok(),
                allowed
            );
        }
    }
    let mut method = AgentMethod::SendAgentMessage {
        space: SpaceSelector::id(&workspace.id),
        message: "hello".into(),
        attachments: Vec::new(),
        sender_chat: None,
    };
    assert!(
        scope::authorize(&manager, Some(&"scope".into()), &mut method)
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
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
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
async fn a_message_to_one_space_says_when_its_agent_reads_it() {
    let (temp, manager) = manager().await;
    let running = workspace(&temp, &manager, "running").await;
    let _execution = run_agent(&manager, &running).await;
    let create = |name: &str| {
        manager.create_workspace(&running.repository_id, name.into(), None, None, None)
    };
    let queued = create("queued").await.unwrap();
    // Queued without the check that claude is on PATH, which CI lacks.
    let launch = crate::protocol::operations::QueuedLaunch::new("claude".into());
    let request = crate::start_plan::queued_agent_request(&queued, launch, None).unwrap();
    manager.queue_add(request).await.unwrap();
    let without = create("without").await.unwrap();
    let repo = manager.repository(&running.repository_id).await.unwrap();
    let chat =
        crate::daemon::test_support::chat(&manager, "planning", "chat", Some(&repo), None).await;
    let send = |id: &SpaceId| {
        let selector = SpaceSelector::id(id);
        let manager = &manager;
        async move {
            manager
                .send_space_message(&selector, "Rebase".into(), Vec::new(), &Attacher::User)
                .await
                .unwrap()
        }
    };
    fn ids(spaces: &crate::client::output::messages::ReachedSpaces) -> Vec<&SpaceId> {
        let spaces = spaces.workspaces.iter().chain(&spaces.chats);
        spaces.map(|space| &space.id).collect()
    }
    let reached = send(&running.id).await;
    assert_eq!(reached.now.workspaces[0].number, running.number);
    assert_eq!(ids(&reached.now), [&running.id]);
    let reached = send(&queued.id).await;
    assert_eq!(ids(&reached.on_start), [&queued.id]);
    let reached = send(&without.id).await;
    assert_eq!(ids(&reached.without_agent), [&without.id]);
    assert_eq!(
        (ids(&reached.now), ids(&reached.on_start)),
        (vec![], vec![])
    );
    let reached = send(&chat.id).await;
    assert_eq!(names(&reached.now.chats), ["planning"]);
    // The space without an agent holds the message for one that starts.
    let messages = manager
        .agent_messages(&SpaceSelector::id(&without.id))
        .await
        .unwrap();
    assert_eq!(messages.len(), 1);
}

#[tokio::test]
async fn a_message_to_all_reaches_running_agents_and_chats_below_the_sender() {
    let (temp, manager) = manager().await;
    let parent = workspace(&temp, &manager, "parent").await;
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
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
    // Chats run with this process as their agent.
    let mut chats = Vec::new();
    for (repository, name) in [
        (&parent.repository_id, "planning"),
        (&parent.repository_id, "sender"),
        (&elsewhere.id, "other"),
    ] {
        let chat = manager
            .start_chat(
                repository.clone().into(),
                name.into(),
                "claude".into(),
                None,
            )
            .await
            .unwrap();
        chats.push(chat);
    }
    let sender = chats[1].id.clone();
    let send = |repository: Option<&RepositoryId>, below: Option<SpaceId>| {
        let (manager, sender) = (&manager, &sender);
        let repository = repository.cloned().map(Into::into);
        async move {
            manager
                .send_all_message(
                    repository.as_ref(),
                    "Rebase onto main".into(),
                    Vec::new(),
                    &Attacher::User,
                    below.as_ref(),
                    Some(sender),
                )
                .await
                .unwrap()
        }
    };
    let reached = send(None, None).await.now;
    assert_eq!(
        (names(&reached.workspaces), names(&reached.chats)),
        (
            vec!["child", "outside", "parent", "sibling"],
            vec!["other", "planning"]
        )
    );
    let reached = send(Some(&parent.repository_id), None).await.now;
    assert_eq!(names(&reached.workspaces), ["child", "parent", "sibling"]);
    // The sending chat is left out.
    assert_eq!(names(&reached.chats), ["planning"]);
    // The repeat collapses into the unread message.
    for workspace in [&parent, &child, &sibling, &outside] {
        let messages = manager
            .agent_messages(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap();
        assert_eq!(messages.len(), 1, "{}", workspace.name);
    }
    assert!(
        manager
            .agent_messages(&SpaceSelector::id(&idle.id))
            .await
            .unwrap()
            .is_empty()
    );
    let reached = send(None, Some(parent.id.clone())).await;
    assert_eq!(
        (names(&reached.now.workspaces), names(&reached.now.chats)),
        (vec!["child"], vec![])
    );
    assert!(send(None, Some(child.id.clone())).await.is_empty());
    // A workspace process names only its own repository, or none.
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: parent.id.clone(),
                execution: None,
            },
        )
        .await;
    for (repository, allowed) in [
        (Some(&parent.repository_id), true),
        (Some(&elsewhere.id), false),
        (None, true),
    ] {
        let mut method = AgentMethod::SendAllAgentMessage {
            repository: repository.cloned().map(Into::into),
            message: "Rebase onto main".into(),
            attachments: Vec::new(),
            sender_chat: None,
        };
        assert_eq!(
            scope::authorize(&manager, Some(&"scope".into()), &mut method)
                .await
                .is_ok(),
            allowed
        );
    }
}

#[tokio::test]
async fn a_message_to_all_waits_for_stopped_and_queued_agents_without_starting_them() {
    let (temp, manager) = manager().await;
    let running = workspace(&temp, &manager, "running").await;
    let _execution = run_agent(&manager, &running).await;
    let create = |name: &str| {
        manager.create_workspace(&running.repository_id, name.into(), None, None, None)
    };
    let stopped = create("stopped").await.unwrap();
    let queued = create("queued").await.unwrap();
    let without = create("without").await.unwrap();
    // Its agent's wrapper waits to reattach, as after a daemon restart.
    let reattaching = create("reattaching").await.unwrap();
    let execution = manager
        .begin_execution(reattaching.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE executions SET agent='claude' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    // The user stopped its agent, which `shoal restore` restores.
    crate::execution::recovery::LastSession {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        execution: "stopped".into(),
        agent: "claude".into(),
        session: None,
        moved: false,
        copied: None,
        origin: None,
        stopped_by: None,
        stop: None,
        converted: false,
    }
    .write(&manager.paths, &stopped.id)
    .unwrap();
    // A record that does not parse leaves the broadcast to the others.
    std::fs::write(
        manager
            .paths
            .space_state(&stopped.id)
            .join("lost.command.json"),
        "{",
    )
    .unwrap();
    // Its queued agent has not read the messages that fill its queue, and
    // it comes first, so the spaces after it show the broadcast goes on.
    let full = create("full").await.unwrap();
    // Queued without the check that claude is on PATH, which CI lacks.
    for workspace in [&queued, &full] {
        let launch = crate::protocol::operations::QueuedLaunch::new("claude".into());
        let request = crate::start_plan::queued_agent_request(workspace, launch, None).unwrap();
        manager.queue_add(request).await.unwrap();
    }
    for n in 0..super::MAX_UNREAD {
        manager
            .queue_agent_message(&full.id, format!("Message {n}"))
            .await
            .unwrap();
    }
    for name in ["planning", "paused"] {
        manager
            .start_chat(
                running.repository_id.clone().into(),
                name.into(),
                "claude".into(),
                None,
            )
            .await
            .unwrap();
    }
    let chats = manager.chats().await.unwrap();
    let paused = chats.iter().find(|chat| chat.name == "paused").unwrap();
    manager.end_executions(&paused.id).await;
    let reached = manager
        .send_all_message(
            None,
            "Rebase onto main".into(),
            Vec::new(),
            &Attacher::User,
            None,
            None,
        )
        .await
        .unwrap();
    let [refused] = reached.refused.workspaces.as_slice() else {
        panic!("{:?}", reached.refused);
    };
    assert_eq!(refused.space.id, full.id);
    assert!(refused.error.contains("unread"), "{}", refused.error);
    let [running_space] = reached.now.workspaces.as_slice() else {
        panic!("{:?}", reached.now);
    };
    assert_eq!(
        (running_space.number, &running_space.id),
        (running.number, &running.id)
    );
    assert_eq!(names(&reached.now.chats), ["planning"]);
    assert_eq!(
        names(&reached.on_start.workspaces),
        ["queued", "reattaching", "stopped"]
    );
    assert_eq!(names(&reached.on_start.chats), ["paused"]);
    for (workspace, unread) in [(&stopped, 1), (&queued, 1), (&without, 0)] {
        let messages = manager
            .agent_messages(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap();
        assert_eq!(messages.len(), unread, "{}", workspace.name);
    }
    // Only the launches queued before wait in the queue.
    assert_eq!(
        manager.queued_agent_workspaces().await.unwrap(),
        [queued.id.clone(), full.id.clone()].into()
    );
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
                &SpaceSelector::id(&workspace.id),
                "See the log".into(),
                vec![file.clone().into()],
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    manager
        .send_agent_message(
            &SpaceSelector::id(&workspace.id),
            String::new(),
            vec![file.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    std::fs::write(&file, "changed").unwrap();
    let messages = manager
        .agent_messages(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
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
        .mark_agent_messages_delivered(&SpaceSelector::id(&workspace.id), vec![attached.id])
        .await
        .unwrap();
    assert!(copy.exists(), "a delivered message keeps its files");
    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
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
                &SpaceSelector::id(&workspace.id),
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
                &SpaceSelector::id(&workspace.id),
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
        let (manager, id) = (manager.clone(), workspace.id.clone());
        async move {
            manager
                .send_agent_message(
                    &SpaceSelector::id(&id),
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
            .agent_messages(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .is_empty()
    );
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    let attachments = repo.workspaces_dir.unwrap().join(".attachments");
    assert!(!attachments.join(&workspace.id).exists());
    drop(held);
    send.await.unwrap().unwrap();
    let messages = manager
        .agent_messages(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
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
                    &SpaceSelector::id(&id),
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
                    &SpaceSelector::id(&id),
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
        let sent = manager
            .send_agent_message(
                &SpaceSelector::id(&workspace.id),
                "See".into(),
                vec![refused.clone().into()],
                &Attacher::User,
            )
            .await;
        assert!(sent.is_err(), "{}", refused.display());
    }
    assert!(
        manager
            .send_agent_message(
                &SpaceSelector::id(&workspace.id),
                String::new(),
                Vec::new(),
                &Attacher::User
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .agent_messages(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .is_empty()
    );
}
