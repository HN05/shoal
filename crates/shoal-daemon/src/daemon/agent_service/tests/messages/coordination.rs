use crate::removal::{InspectionPolicy, UnsavedWork};
use crate::{
    daemon::{
        manager::Manager,
        notifications::{AlertKind, Source},
        test_support::{manager, repository},
    },
    model::{Chat, Workspace},
    state::AgentState,
};
use shoal_protocol::space_names::SpaceSelector;

/// A repository with a workspace named `worker` and a chat whose process is
/// this test.
async fn chat_and_worker(
    temp: &tempfile::TempDir,
    manager: &std::sync::Arc<Manager>,
) -> (Chat, Workspace) {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.into(), "chat-1".into(), "claude".into(), None)
        .await
        .unwrap();
    manager
        .spaces
        .assign_parent(&**manager, &worker.id, Some(chat.id.clone()))
        .await
        .unwrap();
    (chat, worker)
}

async fn received(manager: &Manager, chat: &Chat) -> Vec<String> {
    let messages = manager
        .agents
        .agent_messages(manager, &SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    // As the agent reads them, after the space each names.
    (messages.into_iter())
        .map(|m| match m.space_label() {
            Some(space) => format!("{space}: {}", m.message),
            None => m.message,
        })
        .collect()
}

async fn user(manager: &Manager) -> Vec<(Option<String>, AlertKind, String)> {
    let notifications = manager.notifications.list(false, 100).await.unwrap();
    notifications
        .into_iter()
        .map(|n| (n.space.map(String::from), n.kind, n.message))
        .collect()
}

#[tokio::test]
async fn a_chat_coordinates_the_workspaces_it_owns_below_their_agents() {
    let (temp, manager) = manager().await;
    let (chat, worker) = chat_and_worker(&temp, &manager).await;
    let helper = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&worker.id),
            "helper".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    // Without an agent of its own, the worker is the chat's or the user's
    // own work, which the user hears about.
    manager
        .coordination
        .send_alert(
            &*manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &worker.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "first".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&worker.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&helper.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .coordination
        .send_alert(
            &*manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &worker.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "second".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    // The worker's agent coordinates the helper below it, not the chat.
    manager
        .coordination
        .send_alert(
            &*manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &helper.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "to the worker".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        received(&manager, &chat).await,
        [format!("{} worker: second", worker.number)]
    );
    let worker_messages = manager
        .agents
        .agent_messages(&*manager, &SpaceSelector::id(&worker.id))
        .await
        .unwrap();
    assert_eq!(
        worker_messages[0].text(),
        format!("{} helper: to the worker", helper.number)
    );
    assert_eq!(
        user(&manager).await,
        [(Some("worker".into()), AlertKind::Message, "first".into())]
    );

    // Once the worker's agent has gone, the chat that owns the worker
    // coordinates the helper too.
    let id = worker.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM space_agent_state WHERE space_id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    manager
        .coordination
        .send_alert(
            &*manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &helper.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "to the chat".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        received(&manager, &chat).await,
        [
            format!("{} worker: second", worker.number),
            format!("{} helper: to the chat", helper.number)
        ]
    );
}

#[tokio::test]
async fn delivery_deletes_chat_messages_and_replaces_a_running_wait() {
    let (temp, manager) = manager().await;
    let (chat, worker) = chat_and_worker(&temp, &manager).await;
    manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&worker.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();

    let manager = std::sync::Arc::new(manager);
    let waiting = tokio::spawn({
        let (manager, chat) = (manager.clone(), chat.id.clone());
        async move {
            manager
                .links
                .wait_space_updates(&**manager, &SpaceSelector::id(&chat))
                .await
        }
    });
    manager
        .coordination
        .send_alert(
            &**manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &worker.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "done with the tests".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    let woken = waiting.await.unwrap().unwrap();
    assert!(!woken.superseded);
    assert_eq!(
        woken.alerts[0].text(),
        format!("{} worker: done with the tests", worker.number)
    );

    let ids = woken.alerts.iter().map(|m| m.id).collect();
    manager
        .agents
        .mark_agent_messages_delivered(&**manager, &SpaceSelector::id(&chat.id), ids)
        .await
        .unwrap();
    let unread = (manager.store)
        .read(|db| crate::daemon::messages::every_unread(db))
        .await
        .unwrap();
    assert!(
        (unread.iter()).all(|alert| alert.recipient.as_ref().is_none_or(|to| to.id != chat.id))
    );

    // A hook delivery replaces a wait still running, which leaves the next
    // messages to the delivery.
    let waiting = tokio::spawn({
        let (manager, chat) = (manager.clone(), chat.id.clone());
        async move {
            manager
                .links
                .wait_space_updates(&**manager, &SpaceSelector::id(&chat))
                .await
        }
    });
    while !manager.links.waits().is_waiting(&chat.id) {
        tokio::task::yield_now().await;
    }
    assert!(
        manager
            .links
            .agent_updates(&**manager, &SpaceSelector::id(&chat.id))
            .await
            .unwrap()
            .alerts
            .is_empty()
    );
    assert!(waiting.await.unwrap().unwrap().superseded);
    manager
        .coordination
        .send_alert(
            &**manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &worker.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "next".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    let delivered = manager
        .links
        .agent_updates(&**manager, &SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    assert_eq!(
        delivered.alerts[0].text(),
        format!("{} worker: next", worker.number)
    );
}

#[tokio::test]
async fn the_user_messages_a_chat_without_naming_a_workspace() {
    let (temp, manager) = manager().await;
    let (chat, _) = chat_and_worker(&temp, &manager).await;
    let manager = std::sync::Arc::new(manager);
    let waiting = tokio::spawn({
        let (manager, chat) = (manager.clone(), chat.id.clone());
        async move {
            manager
                .links
                .wait_space_updates(&**manager, &SpaceSelector::id(&chat))
                .await
        }
    });
    manager
        .coordination
        .send_space_message(
            &**manager,
            None,
            &SpaceSelector::id(&chat.id),
            "Pick a new name".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    let woken = waiting.await.unwrap().unwrap();
    assert_eq!(woken.alerts[0].message, "Pick a new name");

    // Sent again before the chat reads it, by ID this time, it collapses.
    manager
        .coordination
        .send_space_message(
            &**manager,
            None,
            &SpaceSelector::id(&chat.id),
            "Pick a new name".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(received(&manager, &chat).await, ["Pick a new name"]);

    manager
        .coordination
        .send_space_message(
            &**manager,
            None,
            &"missing".into(),
            "Pick a new name".into(),
            Vec::new(),
        )
        .await
        .unwrap_err();
}

#[tokio::test]
async fn a_removed_chat_gives_the_user_the_events_it_had_not_received() {
    let (temp, manager) = manager().await;
    let (_, worker) = chat_and_worker(&temp, &manager).await;
    let chat = manager
        .start_chat(
            worker.repository().clone().into(),
            "chat-2".into(),
            "codex".into(),
            None,
        )
        .await
        .unwrap();
    manager
        .spaces
        .assign_parent(&*manager, &worker.id, Some(chat.id.clone()))
        .await
        .unwrap();
    manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&worker.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .coordination
        .send_alert(
            &*manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &worker.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "unread".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    // Ready marks reach only a coordinator, never the user.
    assert!(
        (manager
            .coordination
            .tell_coordinator(
                &*manager,
                crate::daemon::alerts::NewAlert::shoal(
                    AlertKind::ReadyMarked,
                    crate::daemon::alerts::About::space(&worker.name, &worker.id),
                    "marked ready".into(),
                ),
                false,
                Source::Agent,
            )
            .await)
            .is_some()
    );
    assert_eq!(
        received(&manager, &chat).await,
        [
            format!("{} worker: unread", worker.number),
            format!("{} worker: marked ready", worker.number)
        ]
    );
    assert!(user(&manager).await.is_empty());

    // A stopped chat keeps them for its restore.
    manager.agents.end_executions(&*manager, &chat.id).await;
    assert_eq!(
        received(&manager, &chat).await,
        [
            format!("{} worker: unread", worker.number),
            format!("{} worker: marked ready", worker.number)
        ]
    );
    assert!(user(&manager).await.is_empty());
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&chat.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let unread = (manager.store)
        .read(|db| crate::daemon::messages::every_unread(db))
        .await
        .unwrap();
    assert!(
        (unread.iter()).all(|alert| alert.recipient.as_ref().is_none_or(|to| to.id != chat.id))
    );
    let unread = (
        Some("worker".to_owned()),
        AlertKind::Message,
        "unread".to_owned(),
    );
    assert_eq!(user(&manager).await, [unread]);
    // The worker went first, with the chat it was below.
    assert!(manager.spaces.workspace_by_id(&worker.id).await.is_err());
}

#[tokio::test]
async fn a_removed_workspace_gives_the_user_the_events_it_had_not_received() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
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
    for space in [&parent, &child] {
        manager
            .agents
            .set_agent_state(
                &*manager,
                &SpaceSelector::id(&space.id),
                AgentState::Working,
                None,
                None,
            )
            .await
            .unwrap();
    }
    manager
        .coordination
        .send_alert(
            &*manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &child.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "unread".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    // Shoal's own messages stand for no notification.
    manager
        .agents
        .queue_agent_message(
            &parent.id,
            crate::daemon::alerts::AlertKind::Message,
            "from Shoal".into(),
        )
        .await
        .unwrap();
    assert!(user(&manager).await.is_empty());

    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&parent.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let messages: Vec<_> = (user(&manager).await.into_iter())
        .filter(|(_, kind, _)| *kind == AlertKind::Message)
        .collect();
    let unread = (
        Some("child".to_owned()),
        AlertKind::Message,
        "unread".to_owned(),
    );
    assert_eq!(messages, [unread]);
}

#[tokio::test]
async fn a_removed_chat_keeps_who_sent_the_events_it_hands_over() {
    let (temp, manager) = manager().await;
    let (_, worker) = chat_and_worker(&temp, &manager).await;
    let chat = manager
        .start_chat(
            worker.repository().clone().into(),
            "chat-3".into(),
            "codex".into(),
            None,
        )
        .await
        .unwrap();
    manager
        .spaces
        .assign_parent(&*manager, &worker.id, Some(chat.id.clone()))
        .await
        .unwrap();
    manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&worker.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .coordination
        .send_alert(
            &*manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &worker.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "unread".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    // The worker is renamed and a new workspace takes its name before the
    // chat is removed.
    manager
        .spaces
        .rename_branch(&*manager, &SpaceSelector::id(&worker.id), "renamed", None)
        .await
        .unwrap();
    let successor = manager
        .create_workspace(
            worker.repository(),
            "worker".into(),
            None,
            None,
            Some(temp.path().join("successor")),
        )
        .await
        .unwrap();

    manager.agents.end_executions(&*manager, &chat.id).await;
    // Opened outside the chat, the worker is the user's and stays when the
    // chat is removed.
    manager
        .spaces
        .assign_parent(&*manager, &worker.id, None)
        .await
        .unwrap();
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&chat.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let handed = manager.notifications.list(true, 50).await.unwrap();
    assert_eq!(handed.len(), 1);
    let id = handed[0].id;
    assert!(
        manager
            .coordination
            .withdraw_alert(
                &*manager,
                Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                    &successor.id
                ))),
                None,
                id
            )
            .await
            .is_err()
    );
    manager
        .coordination
        .withdraw_alert(
            &*manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &worker.id,
            ))),
            None,
            id,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn an_ended_chat_hands_over_the_files_sent_with_its_events_where_they_moved() {
    let (temp, manager) = manager().await;
    let (_, worker) = chat_and_worker(&temp, &manager).await;
    let chat = manager
        .start_chat(
            worker.repository().clone().into(),
            "chat-4".into(),
            "codex".into(),
            None,
        )
        .await
        .unwrap();
    manager
        .spaces
        .assign_parent(&*manager, &worker.id, Some(chat.id.clone()))
        .await
        .unwrap();
    manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&worker.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    let shot = temp.path().join("layout.png");
    std::fs::write(&shot, "png").unwrap();
    manager
        .coordination
        .send_alert(
            &*manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &worker.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "Which layout?".into(),
            vec![shot.into()],
        )
        .await
        .unwrap();
    let messages = manager
        .agents
        .agent_messages(&*manager, &SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    let attachments = messages[0].attachments();
    let [copy] = attachments.as_slice() else {
        panic!("{messages:?}");
    };
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "png");

    // The worker's repository directory moves, as a chat of another
    // repository may coordinate it.
    let repo = manager
        .repositories
        .repository(worker.repository())
        .await
        .unwrap();
    let source = repo.workspaces_dir;
    let destination = temp.path().join("moved");
    std::fs::rename(&source, &destination).unwrap();
    let (from, to) = (source.clone(), destination.clone());
    let id = worker.repository().clone();
    manager
        .store
        .write(move |tx| {
            crate::daemon::workspace_files::rebase(tx, &id, &from, &to)?;
            crate::daemon::alerts::rebase_attachments(tx, &from, &to)?;
            Ok(())
        })
        .await
        .unwrap();
    let messages = manager
        .agents
        .agent_messages(&*manager, &SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    let attachments = messages[0].attachments();
    let [moved] = attachments.as_slice() else {
        panic!("{messages:?}");
    };
    assert_eq!(
        *moved,
        destination.join(copy.strip_prefix(&source).unwrap())
    );

    // Removed with nothing below it, the chat hands the files to the user.
    manager.agents.end_executions(&*manager, &chat.id).await;
    manager
        .spaces
        .assign_parent(&*manager, &worker.id, None)
        .await
        .unwrap();
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&chat.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let notifications = manager.notifications.list(false, 100).await.unwrap();
    let [notification] = notifications.as_slice() else {
        panic!("{notifications:?}");
    };
    assert_eq!(notification.message, "Which layout?");
    let [file] = notification.files.as_slice() else {
        panic!("{notification:?}");
    };
    assert_eq!(file.path().unwrap(), *moved);
    assert_eq!(
        std::fs::read_to_string(file.path().unwrap()).unwrap(),
        "png"
    );
}

#[tokio::test]
async fn same_named_spaces_reach_their_own_coordinators() {
    let (temp, manager) = manager().await;
    let (chat, worker) = chat_and_worker(&temp, &manager).await;
    let other_chat = manager
        .start_chat(
            worker.repository().clone().into(),
            "chat-2".into(),
            "codex".into(),
            None,
        )
        .await
        .unwrap();
    let other = manager
        .create_workspace(worker.repository(), "other".into(), None, None, None)
        .await
        .unwrap();
    manager
        .spaces
        .assign_parent(&*manager, &other.id, Some(other_chat.id.clone()))
        .await
        .unwrap();
    // Both take the same name, which identifies neither.
    for workspace in [&worker, &other] {
        let selector = SpaceSelector::id(&workspace.id);
        (manager
            .spaces
            .rename_space(&selector, &"Same work".parse().unwrap()))
        .await
        .unwrap();
        (manager
            .agents
            .set_agent_state(&*manager, &selector, AgentState::Working, None, None))
        .await
        .unwrap();
    }
    for (workspace, message) in [(&worker, "from the worker"), (&other, "from the other")] {
        manager
            .coordination
            .send_alert(
                &*manager,
                Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                    &workspace.id,
                ))),
                crate::protocol::AlertTarget::Up,
                message.into(),
                Vec::new(),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        received(&manager, &chat).await,
        [format!("{} Same work: from the worker", worker.number)]
    );
    assert_eq!(
        received(&manager, &other_chat).await,
        [format!("{} Same work: from the other", other.number)]
    );
    assert!(user(&manager).await.is_empty());
}

#[tokio::test]
async fn same_named_siblings_send_the_same_message_apart() {
    let (temp, manager) = manager().await;
    let (chat, worker) = chat_and_worker(&temp, &manager).await;
    let sibling = manager
        .create_workspace(worker.repository(), "sibling".into(), None, None, None)
        .await
        .unwrap();
    manager
        .spaces
        .assign_parent(&*manager, &sibling.id, Some(chat.id.clone()))
        .await
        .unwrap();
    for workspace in [&worker, &sibling] {
        let selector = SpaceSelector::id(&workspace.id);
        (manager
            .spaces
            .rename_space(&selector, &"Same work".parse().unwrap()))
        .await
        .unwrap();
        (manager
            .agents
            .set_agent_state(&*manager, &selector, AgentState::Working, None, None))
        .await
        .unwrap();
    }
    for workspace in [&worker, &sibling] {
        manager
            .coordination
            .send_alert(
                &*manager,
                Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                    &workspace.id,
                ))),
                crate::protocol::AlertTarget::Up,
                "tests pass".into(),
                Vec::new(),
            )
            .await
            .unwrap();
    }
    // Identical text from two spaces is two messages, each naming its own.
    assert_eq!(
        received(&manager, &chat).await,
        [
            format!("{} Same work: tests pass", worker.number),
            format!("{} Same work: tests pass", sibling.number),
        ]
    );
}
