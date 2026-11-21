use super::Delivery;
use crate::execution::recovery::LastSessionExt;
use crate::paths::StateFiles;
use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        scope::{self, Caller},
        test_support::{manager, names, repository},
    },
    protocol::{AlertMethod, AlertTarget, AlertsFor},
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

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
    let changed = manager.signals.watch_messages();
    for _ in 0..2 {
        manager
            .send_space_message(
                None,
                &SpaceSelector::id(&workspace.id),
                "Stop the dev server".into(),
                Vec::new(),
            )
            .await
            .unwrap();
    }
    assert!(changed.has_changed().unwrap());
    manager
        .send_space_message(
            None,
            &SpaceSelector::id(&workspace.id),
            "Rebase onto main".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    manager
        .send_space_message(
            None,
            &SpaceSelector::id(&workspace.id),
            "Move:\n\tabc123 to api".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    for rejected in [
        "\x1b]9;spoof\x07".to_owned(),
        "x".repeat(crate::validate::MAX_AGENT_MESSAGE_LEN + 1),
    ] {
        assert!(
            manager
                .send_space_message(
                    None,
                    &SpaceSelector::id(&workspace.id),
                    rejected,
                    Vec::new(),
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
        .send_space_message(
            None,
            &SpaceSelector::id(&workspace.id),
            "Stop the dev server".into(),
            Vec::new(),
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
        manager.queue_agent_message_delivered(
            &workspace.id,
            crate::daemon::alerts::AlertKind::Message,
            "Memory is high".into(),
            delivery,
        )
    };
    let wakes = || async { manager.unread_messages(&workspace.id).await.unwrap().wakes };
    // Work running in the workspace does not make it wake.
    let _command = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    queue(Delivery::NextTurn).await.unwrap();
    assert!(!wakes().await);
    let mut changed = manager.signals.watch_messages();
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
        .create_workspace(workspace.repository(), "other".into(), None, None, None)
        .await
        .unwrap();
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: workspace.id.clone(),
                execution: None,
                granted: None,
                read_only: false,
                vouched: None,
            },
        )
        .await;
    for (target, allowed) in [(&workspace.id, true), (&other.id, false)] {
        let mut method = AlertMethod::ListAlerts {
            to: AlertsFor::Space {
                space: SpaceSelector::from(target),
            },
            unread_only: true,
            limit: 50,
        };
        assert_eq!(
            scope::authorize(&manager, Some(&"scope".into()), &mut method)
                .await
                .is_ok(),
            allowed
        );
    }
    // A process sends its own space's agent nothing: its alerts go up.
    let mut method = AlertMethod::SendAlert {
        from: None,
        to: AlertTarget::Space {
            space: SpaceSelector::id(&workspace.id),
        },
        message: "hello".into(),
        attachments: Vec::new(),
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
) -> crate::daemon::executions::StartedExecution {
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    manager
        .track_agent(
            &execution.plan.id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
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
    let create =
        |name: &str| manager.create_workspace(running.repository(), name.into(), None, None, None);
    let queued = create("queued").await.unwrap();
    // Queued without the check that claude is on PATH, which CI lacks.
    let launch = crate::protocol::operations::QueuedLaunch::new("claude".into());
    let request = crate::start_plan::queued_agent_request(&queued, launch, None).unwrap();
    manager.queue_add(request).await.unwrap();
    let without = create("without").await.unwrap();
    let repo = manager.repository(running.repository()).await.unwrap();
    let chat =
        crate::daemon::test_support::chat(&manager, "planning", "chat", Some(&repo), None).await;
    let send = |id: &SpaceId| {
        let selector = SpaceSelector::id(id);
        let manager = &manager;
        async move {
            manager
                .send_space_message(None, &selector, "Rebase".into(), Vec::new())
                .await
                .unwrap()
        }
    };
    fn ids(spaces: &shoal_protocol::alerts::ReachedSpaces) -> Vec<&SpaceId> {
        let spaces = spaces.workspaces.iter().chain(&spaces.chats);
        spaces.map(|space| &space.id).collect()
    }
    let reached = send(&running.id).await;
    assert_eq!(reached.reached.now.workspaces[0].number, running.number);
    assert_eq!(ids(&reached.reached.now), [&running.id]);
    let reached = send(&queued.id).await;
    assert_eq!(ids(&reached.reached.on_start), [&queued.id]);
    let reached = send(&without.id).await;
    assert_eq!(ids(&reached.reached.without_agent), [&without.id]);
    assert_eq!(
        (ids(&reached.reached.now), ids(&reached.reached.on_start)),
        (vec![], vec![])
    );
    let reached = send(&chat.id).await;
    assert_eq!(names(&reached.reached.now.chats), ["planning"]);
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
        .create_child(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let sibling = manager
        .create_workspace(parent.repository(), "sibling".into(), None, None, None)
        .await
        .unwrap();
    let idle = manager
        .create_workspace(parent.repository(), "idle".into(), None, None, None)
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
        (parent.repository(), "planning"),
        (parent.repository(), "sender"),
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
    // The user reaches every space; a workspace's process only those below
    // its own, but its own.
    let send = |repository: Option<&RepositoryId>, below: Option<SpaceId>| {
        let caller = below.map(scoped);
        let manager = &manager;
        let repository = repository.cloned().map(Into::into);
        async move {
            manager
                .send_all_message(
                    caller.as_ref(),
                    repository.as_ref(),
                    "Rebase onto main".into(),
                    Vec::new(),
                )
                .await
                .unwrap()
        }
    };
    let reached = send(None, None).await.reached.now;
    assert_eq!(
        (names(&reached.workspaces), names(&reached.chats)),
        (
            vec!["child", "outside", "parent", "sibling"],
            vec!["other", "planning", "sender"]
        )
    );
    let reached = send(parent.repository_id.as_ref(), None).await.reached.now;
    assert_eq!(names(&reached.workspaces), ["child", "parent", "sibling"]);
    assert_eq!(names(&reached.chats), ["planning", "sender"]);
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
        (
            names(&reached.reached.now.workspaces),
            names(&reached.reached.now.chats)
        ),
        (vec!["child"], vec![])
    );
    assert!(send(None, Some(child.id.clone())).await.reached.is_empty());
    // A workspace process names only its own repository, or none.
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: parent.id.clone(),
                execution: None,
                granted: None,
                read_only: false,
                vouched: None,
            },
        )
        .await;
    for (repository, allowed) in [
        (Some(parent.repository()), true),
        (Some(&elsewhere.id), false),
        (None, true),
    ] {
        let mut method = AlertMethod::SendAlert {
            from: None,
            to: AlertTarget::All {
                repository: repository.cloned().map(Into::into),
            },
            message: "Rebase onto main".into(),
            attachments: Vec::new(),
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
async fn a_message_to_all_from_a_coordinator_reaches_the_chats_below_it_not_being_removed() {
    let (temp, manager) = manager().await;
    let parent = workspace(&temp, &manager, "parent").await;
    let mut chats = Vec::new();
    for (name, below) in [("below", true), ("removing", true), ("beside", false)] {
        let new = crate::daemon::chats::NewChat {
            priority: None,
            narrowing: Default::default(),
            name: name.into(),
            agent: "claude".into(),
            mode: crate::model::chat_mode(),
            origin: None,
            parent: below.then(|| parent.id.clone()),
        };
        let repository = Some(parent.repository().clone().into());
        let chat = manager.start_new_chat(repository, None, new).await.unwrap();
        chats.push(manager.run_chat_agent(chat, None).await.unwrap());
    }
    let _removal = manager.space_removals.reserve(&chats[1].id).unwrap();
    let reached = manager
        .send_all_message(
            Some(&scoped(parent.id.clone())),
            None,
            "Rebase onto main".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(names(&reached.reached.now.chats), ["below"]);
    assert!(reached.reached.now.workspaces.is_empty());
    assert!(reached.reached.on_start.is_empty() && reached.reached.refused.is_empty());
}

#[tokio::test]
async fn a_message_to_all_waits_for_stopped_and_queued_agents_without_starting_them() {
    let (temp, manager) = manager().await;
    let running = workspace(&temp, &manager, "running").await;
    let _execution = run_agent(&manager, &running).await;
    let create =
        |name: &str| manager.create_workspace(running.repository(), name.into(), None, None, None);
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
    // The user stopped its agent, which `shoal resume` restores.
    crate::execution::recovery::LastSession {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        turn: None,
        execution: "stopped".into(),
        agent: "claude".into(),
        session: None,
        moved: false,
        copied: None,
        origin: None,
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
    for n in 0..shoal_daemon_base::alerts::MAX_UNREAD {
        manager
            .queue_agent_message(
                &full.id,
                crate::daemon::alerts::AlertKind::Message,
                format!("Message {n}"),
            )
            .await
            .unwrap();
    }
    for name in ["planning", "paused"] {
        manager
            .start_chat(
                running.repository().clone().into(),
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
        .send_all_message(None, None, "Rebase onto main".into(), Vec::new())
        .await
        .unwrap();
    let [refused] = reached.reached.refused.workspaces.as_slice() else {
        panic!("{:?}", reached.reached.refused);
    };
    assert_eq!(refused.space.id, full.id);
    assert!(refused.error.contains("unread"), "{}", refused.error);
    let [running_space] = reached.reached.now.workspaces.as_slice() else {
        panic!("{:?}", reached.reached.now);
    };
    assert_eq!(
        (running_space.number, &running_space.id),
        (running.number, &running.id)
    );
    assert_eq!(names(&reached.reached.now.chats), ["planning"]);
    assert_eq!(
        names(&reached.reached.on_start.workspaces),
        ["queued", "reattaching", "stopped"]
    );
    assert_eq!(names(&reached.reached.on_start.chats), ["paused"]);
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
            .send_space_message(
                None,
                &SpaceSelector::id(&workspace.id),
                "See the log".into(),
                vec![file.clone().into()],
            )
            .await
            .unwrap();
    }
    manager
        .send_space_message(
            None,
            &SpaceSelector::id(&workspace.id),
            String::new(),
            vec![file.clone().into()],
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
    let attachments = attached.attachments();
    let [copy] = attachments.as_slice() else {
        panic!("{attached:?}");
    };
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "panic");
    assert_eq!(copy.file_name(), file.file_name());
    let repo = manager.repository(workspace.repository()).await.unwrap();
    let workspaces_dir = repo.workspaces_dir.as_path();
    assert!(copy.starts_with(workspaces_dir.join(".attachments").join(&workspace.id)));

    manager
        .mark_agent_messages_delivered(&SpaceSelector::id(&workspace.id), vec![attached.id])
        .await
        .unwrap();
    assert!(copy.exists(), "a delivered message keeps its files");
    manager
        .remove_selected(
            &SpaceSelector::id(&workspace.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(copy.exists(), "a removed workspace keeps its files");
    manager
        .store
        .run(|db| {
            db.execute("UPDATE removed_space_files SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_results().await.unwrap();
    assert!(!copy.exists());
}

#[tokio::test]
async fn a_refused_message_leaves_no_copies() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    for n in 0..shoal_daemon_base::alerts::MAX_UNREAD {
        manager
            .send_space_message(
                None,
                &SpaceSelector::id(&workspace.id),
                format!("message {n}"),
                Vec::new(),
            )
            .await
            .unwrap();
    }
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    assert!(
        manager
            .send_space_message(
                None,
                &SpaceSelector::id(&workspace.id),
                "See the log".into(),
                vec![file.into()],
            )
            .await
            .is_err()
    );
    let repo = manager.repository(workspace.repository()).await.unwrap();
    let attachments = repo.workspaces_dir.join(".attachments");
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
        .lock_message_files(workspace.repository())
        .await;
    let gate = manager.gates.message_files_gate(workspace.repository());
    let send = tokio::spawn({
        let (manager, id) = (manager.clone(), workspace.id.clone());
        async move {
            manager
                .send_space_message(
                    None,
                    &SpaceSelector::id(&id),
                    "See the log".into(),
                    vec![file.into()],
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
    let repo = manager.repository(workspace.repository()).await.unwrap();
    let attachments = repo.workspaces_dir.join(".attachments");
    assert!(!attachments.join(&workspace.id).exists());
    drop(held);
    send.await.unwrap().unwrap();
    let messages = manager
        .agent_messages(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(messages[0].attachments()[0].exists());
}

#[tokio::test]
async fn a_send_waiting_through_removal_leaves_no_files() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    let held = manager
        .gates
        .lock_message_files(workspace.repository())
        .await;
    let gate = manager.gates.message_files_gate(workspace.repository());
    let send = tokio::spawn({
        let (manager, id) = (manager.clone(), workspace.id.clone());
        async move {
            manager
                .send_space_message(
                    None,
                    &SpaceSelector::id(&id),
                    "See the log".into(),
                    vec![file.into()],
                )
                .await
        }
    });
    let removal = tokio::spawn({
        let (manager, id) = (manager.clone(), workspace.id.clone());
        async move {
            manager
                .remove_selected(
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
    let repo = manager.repository(workspace.repository()).await.unwrap();
    let attachments = repo.workspaces_dir.join(".attachments");
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
            .send_space_message(
                None,
                &SpaceSelector::id(&workspace.id),
                "See".into(),
                vec![refused.clone().into()],
            )
            .await;
        assert!(sent.is_err(), "{}", refused.display());
    }
    assert!(
        manager
            .send_space_message(
                None,
                &SpaceSelector::id(&workspace.id),
                String::new(),
                Vec::new(),
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

mod coordination;

/// A caller scoped to the space `id`.
fn scoped(id: SpaceId) -> crate::daemon::scope::Caller {
    crate::daemon::scope::Caller {
        space_id: id,
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    }
}

/// Queue alerts for `workspace` with the text `text` gives each and the
/// files `files` until the space refuses one; how many it took.
async fn queue_until_refused(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    text: impl Fn(usize) -> String,
    files: Vec<crate::daemon::workspace_files::NewFile>,
) -> usize {
    let to = SpaceSelector::id(&workspace.id);
    let mut sent = 0;
    while (manager.send_space_message(None, &to, text(sent), files.clone()))
        .await
        .is_ok()
    {
        sent += 1;
    }
    sent
}

/// Check that what the agent of `workspace` receives of its unread alerts
/// stays within their limit.
async fn assert_fits_one_delivery(manager: &Manager, workspace: &crate::model::Workspace) {
    let unread = manager.unread_messages(&workspace.id).await.unwrap().alerts;
    let delivered = serde_json::to_vec(&unread).unwrap();
    assert!(
        delivered.len() <= shoal_daemon_base::alerts::MAX_UNREAD_BYTES,
        "{} alerts take {} bytes",
        unread.len(),
        delivered.len()
    );
}

/// Twenty files, each with a long name and the longest title of
/// four-byte characters.
fn long_titled_files(temp: &tempfile::TempDir) -> Vec<crate::daemon::workspace_files::NewFile> {
    titled_files(temp, &"🦀".repeat(crate::validate::MAX_FILE_TITLE_LEN))
}

/// Twenty files, each with a long name and the title `title`.
fn titled_files(
    temp: &tempfile::TempDir,
    title: &str,
) -> Vec<crate::daemon::workspace_files::NewFile> {
    (0..20)
        .map(|n| {
            let path = temp.path().join(format!("{n:02}{}.log", "x".repeat(200)));
            std::fs::write(&path, "log").unwrap();
            crate::daemon::workspace_files::NewFile {
                path,
                title: Some(title.to_owned()),
            }
        })
        .collect()
}

#[tokio::test]
async fn unread_alerts_with_long_titled_files_still_fit_one_delivery() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let files = long_titled_files(&temp);
    let sent = queue_until_refused(&manager, &workspace, |n| format!("logs {n}"), files).await;
    assert!(sent > 0);
    assert_fits_one_delivery(&manager, &workspace).await;
}

#[tokio::test]
async fn unread_alerts_that_json_escapes_still_fit_one_delivery() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, &"🦀".repeat(40)).await;
    let text = |n| format!("{n}{}", "\t".repeat(500));
    assert!(queue_until_refused(&manager, &workspace, text, Vec::new()).await > 0);
    assert_fits_one_delivery(&manager, &workspace).await;
}

#[tokio::test]
async fn an_alert_s_copied_files_count_toward_a_nearly_full_queue() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let text = |n| format!("{n}{}", "x".repeat(1000));
    assert!(queue_until_refused(&manager, &workspace, text, Vec::new()).await > 0);
    let files = long_titled_files(&temp);
    let to = SpaceSelector::id(&workspace.id);
    let attached = manager.send_space_message(None, &to, "logs".into(), files);
    assert!(attached.await.is_err());
    assert_fits_one_delivery(&manager, &workspace).await;
}

#[tokio::test]
async fn a_repeated_alert_s_longer_titles_count_toward_a_nearly_full_queue() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let to = SpaceSelector::id(&workspace.id);
    let send = |files| manager.send_space_message(None, &to, "logs".into(), files);
    send(titled_files(&temp, "log")).await.unwrap();
    let text = |n| format!("{n}{}", "x".repeat(1000));
    assert!(queue_until_refused(&manager, &workspace, text, Vec::new()).await > 0);
    let retitled = long_titled_files(&temp);
    assert!(send(retitled).await.is_err());
    assert_fits_one_delivery(&manager, &workspace).await;
}

#[tokio::test]
async fn an_upward_alert_s_longer_titles_count_toward_its_coordinator_s_queue() {
    let (temp, manager) = manager().await;
    let parent = workspace(&temp, &manager, "parent").await;
    let child = manager
        .create_child(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    // Both run agents, so the child's alerts go to its coordinator.
    for space in [&parent, &child] {
        let working = crate::state::AgentState::Working;
        (manager.set_agent_state(&SpaceSelector::id(&space.id), working, None, None))
            .await
            .unwrap();
    }
    let as_child = crate::daemon::test_support::caller_in(&SpaceSelector::id(&child.id));
    let up = |files| manager.send_alert(Some(&as_child), AlertTarget::Up, "logs".into(), files);
    up(titled_files(&temp, "log")).await.unwrap();
    let text = |n| format!("{n}{}", "x".repeat(1000));
    assert!(queue_until_refused(&manager, &parent, text, Vec::new()).await > 0);
    assert!(up(long_titled_files(&temp)).await.is_err());
    assert_fits_one_delivery(&manager, &parent).await;
}

#[tokio::test]
async fn a_backlog_beyond_the_limit_reaches_its_agent_over_several_deliveries() {
    use crate::daemon::alerts::{self, AlertKind, NewAlert};
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, &"🦀".repeat(40)).await;
    // What an earlier release queued, which the migration keeps whatever
    // it takes once each alert names its recipient.
    let id = workspace.id.clone();
    (manager.store)
        .write(move |tx| {
            for n in 0..50 {
                let text = format!("{n}{}", "\t".repeat(500));
                let alert = NewAlert::shoal(AlertKind::Message, Default::default(), text);
                alerts::insert(tx, Some(&id), &alert, true, false)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let to = SpaceSelector::id(&workspace.id);
    let mut delivered = Vec::new();
    for _ in 0..50 {
        let unread = manager.unread_messages(&workspace.id).await.unwrap().alerts;
        if unread.is_empty() {
            break;
        }
        let bytes = serde_json::to_vec(&unread).unwrap().len();
        assert!(
            bytes <= shoal_daemon_base::alerts::MAX_UNREAD_BYTES,
            "{bytes} bytes"
        );
        let ids: Vec<i64> = unread.iter().map(|alert| alert.id).collect();
        (manager.mark_agent_messages_delivered(&to, ids.clone()))
            .await
            .unwrap();
        delivered.extend(ids);
    }
    assert_eq!(delivered.len(), 50);
}
