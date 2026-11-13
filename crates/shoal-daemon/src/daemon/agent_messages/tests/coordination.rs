use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        notifications::NotificationKind,
        test_support::{manager, repository},
        workspace::{Manager, Source},
        workspace_files::Attacher,
    },
    model::{Chat, Workspace},
    state::AgentState,
};

/// A repository with a workspace named `worker` and a chat whose process is
/// this test.
async fn chat_and_worker(
    temp: &tempfile::TempDir,
    manager: &std::sync::Arc<Manager>,
) -> (Chat, Workspace) {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
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
        .assign_parent(&worker.id, Some(chat.id.clone()))
        .await
        .unwrap();
    (chat, worker)
}

async fn received(manager: &Manager, chat: &Chat) -> Vec<String> {
    let messages = manager
        .agent_messages(&SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    messages.into_iter().map(|m| m.message).collect()
}

async fn user(manager: &Manager) -> Vec<(Option<String>, NotificationKind, String)> {
    let notifications = manager.notifications(false, 100).await.unwrap();
    notifications
        .into_iter()
        .map(|n| (n.workspace.map(String::from), n.kind, n.message))
        .collect()
}

#[tokio::test]
async fn a_chat_coordinates_the_workspaces_it_owns_below_their_agents() {
    let (temp, manager) = manager().await;
    let (chat, worker) = chat_and_worker(&temp, &manager).await;
    let helper = manager
        .split_workspace(
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
        .send_message(
            &SpaceSelector::id(&worker.id),
            "first".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .set_agent_state(
            &SpaceSelector::id(&worker.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .set_agent_state(
            &SpaceSelector::id(&helper.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .send_message(
            &SpaceSelector::id(&worker.id),
            "second".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // The worker's agent coordinates the helper below it, not the chat.
    manager
        .send_message(
            &SpaceSelector::id(&helper.id),
            "to the worker".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(
        received(&manager, &chat).await,
        [format!("{} worker: second", worker.number)]
    );
    let worker_messages = manager
        .agent_messages(&SpaceSelector::id(&worker.id))
        .await
        .unwrap();
    assert_eq!(
        worker_messages[0].message,
        format!("{} helper: to the worker", helper.number)
    );
    assert_eq!(
        user(&manager).await,
        [(
            Some("worker".into()),
            NotificationKind::AgentMessage,
            "first".into()
        )]
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
        .send_message(
            &SpaceSelector::id(&helper.id),
            "to the chat".into(),
            Vec::new(),
            &Attacher::User,
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
        .set_agent_state(
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
        async move { manager.wait_space_updates(&SpaceSelector::id(&chat)).await }
    });
    manager
        .send_message(
            &SpaceSelector::id(&worker.id),
            "done with the tests".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let woken = waiting.await.unwrap().unwrap();
    assert!(!woken.superseded);
    assert_eq!(
        woken.messages[0].message,
        format!("{} worker: done with the tests", worker.number)
    );

    let ids = woken.messages.iter().map(|m| m.id).collect();
    manager
        .mark_agent_messages_delivered(&SpaceSelector::id(&chat.id), ids)
        .await
        .unwrap();
    let unread = (manager.store)
        .read(|db| crate::daemon::agent_messages::every_unread(db))
        .await
        .unwrap();
    assert!(unread.iter().all(|(space, _)| *space != chat.id));

    // A hook delivery replaces a wait still running, which leaves the next
    // messages to the delivery.
    let waiting = tokio::spawn({
        let (manager, chat) = (manager.clone(), chat.id.clone());
        async move { manager.wait_space_updates(&SpaceSelector::id(&chat)).await }
    });
    while !manager.waits.is_waiting(&chat.id) {
        tokio::task::yield_now().await;
    }
    assert!(
        manager
            .agent_updates(&SpaceSelector::id(&chat.id))
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(waiting.await.unwrap().unwrap().superseded);
    manager
        .send_message(
            &SpaceSelector::id(&worker.id),
            "next".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let delivered = manager
        .agent_updates(&SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    assert_eq!(
        delivered.messages[0].message,
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
        async move { manager.wait_space_updates(&SpaceSelector::id(&chat)).await }
    });
    manager
        .send_space_message(
            &SpaceSelector::id(&chat.id),
            "Pick a new name".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let woken = waiting.await.unwrap().unwrap();
    assert_eq!(woken.messages[0].message, "Pick a new name");

    // Sent again before the chat reads it, by ID this time, it collapses.
    manager
        .send_space_message(
            &SpaceSelector::id(&chat.id),
            "Pick a new name".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(received(&manager, &chat).await, ["Pick a new name"]);

    manager
        .send_space_message(
            &"missing".into(),
            "Pick a new name".into(),
            Vec::new(),
            &Attacher::User,
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
            worker.repository_id.clone().into(),
            "chat-2".into(),
            "codex".into(),
            None,
        )
        .await
        .unwrap();
    manager
        .assign_parent(&worker.id, Some(chat.id.clone()))
        .await
        .unwrap();
    manager
        .set_agent_state(
            &SpaceSelector::id(&worker.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .send_message(
            &SpaceSelector::id(&worker.id),
            "unread".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // Ready marks reach only a coordinator, never the user.
    assert!(
        manager
            .tell_coordinator(
                &worker.name,
                Some(&worker.id),
                None,
                "marked ready".into(),
                Vec::new(),
                Source::Agent,
            )
            .await
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
    manager.end_executions(&chat.id).await;
    assert_eq!(
        received(&manager, &chat).await,
        [
            format!("{} worker: unread", worker.number),
            format!("{} worker: marked ready", worker.number)
        ]
    );
    assert!(user(&manager).await.is_empty());
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    let unread = (manager.store)
        .read(|db| crate::daemon::agent_messages::every_unread(db))
        .await
        .unwrap();
    assert!(unread.iter().all(|(space, _)| *space != chat.id));
    let unread = (
        Some("worker".to_owned()),
        NotificationKind::AgentMessage,
        "unread".to_owned(),
    );
    assert_eq!(user(&manager).await, [unread]);
    // The worker went first, with the chat it was below.
    assert!(manager.workspace_by_id(&worker.id).await.is_err());
}

#[tokio::test]
async fn a_removed_workspace_gives_the_user_the_events_it_had_not_received() {
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
        .split_workspace(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    for space in [&parent, &child] {
        manager
            .set_agent_state(
                &SpaceSelector::id(&space.id),
                AgentState::Working,
                None,
                None,
            )
            .await
            .unwrap();
    }
    manager
        .send_message(
            &SpaceSelector::id(&child.id),
            "unread".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // Shoal's own messages stand for no notification.
    manager
        .queue_agent_message(&parent.id, "from Shoal".into())
        .await
        .unwrap();
    assert!(user(&manager).await.is_empty());

    manager
        .remove_workspace(
            &SpaceSelector::id(&parent.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let messages: Vec<_> = (user(&manager).await.into_iter())
        .filter(|(_, kind, _)| *kind == NotificationKind::AgentMessage)
        .collect();
    let unread = (
        Some("child".to_owned()),
        NotificationKind::AgentMessage,
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
            worker.repository_id.clone().into(),
            "chat-3".into(),
            "codex".into(),
            None,
        )
        .await
        .unwrap();
    manager
        .assign_parent(&worker.id, Some(chat.id.clone()))
        .await
        .unwrap();
    manager
        .set_agent_state(
            &SpaceSelector::id(&worker.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .send_message(
            &SpaceSelector::id(&worker.id),
            "unread".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // The worker is renamed and a new workspace takes its name before the
    // chat is removed.
    manager
        .rename_workspace(&SpaceSelector::id(&worker.id), "renamed", None)
        .await
        .unwrap();
    let successor = manager
        .create_workspace(
            &worker.repository_id,
            "worker".into(),
            None,
            None,
            Some(temp.path().join("successor")),
        )
        .await
        .unwrap();

    manager.end_executions(&chat.id).await;
    // Opened outside the chat, the worker is the user's and stays when the
    // chat is removed.
    manager.assign_parent(&worker.id, None).await.unwrap();
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    let handed = manager.notifications(true, 50).await.unwrap();
    assert_eq!(handed.len(), 1);
    let id = handed[0].id;
    assert!(
        manager
            .withdraw_message(&SpaceSelector::id(&successor.id), id)
            .await
            .is_err()
    );
    manager
        .withdraw_message(&SpaceSelector::id(&worker.id), id)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_ended_chat_hands_over_the_files_sent_with_its_events_where_they_moved() {
    let (temp, manager) = manager().await;
    let (_, worker) = chat_and_worker(&temp, &manager).await;
    let chat = manager
        .start_chat(
            worker.repository_id.clone().into(),
            "chat-4".into(),
            "codex".into(),
            None,
        )
        .await
        .unwrap();
    manager
        .assign_parent(&worker.id, Some(chat.id.clone()))
        .await
        .unwrap();
    manager
        .set_agent_state(
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
        .send_message(
            &SpaceSelector::id(&worker.id),
            "Which layout?".into(),
            vec![shot.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let messages = manager
        .agent_messages(&SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    let [copy] = messages[0].attachments.as_slice() else {
        panic!("{messages:?}");
    };
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "png");

    // The worker's repository directory moves, as a chat of another
    // repository may coordinate it.
    let repo = manager.repository(&worker.repository_id).await.unwrap();
    let source = repo.workspaces_dir.unwrap();
    let destination = temp.path().join("moved");
    std::fs::rename(&source, &destination).unwrap();
    let (from, to) = (source.clone(), destination.clone());
    let id = worker.repository_id.clone();
    manager
        .store
        .write(move |tx| {
            crate::daemon::workspace_files::rebase(tx, &id, &from, &to)?;
            crate::daemon::agent_messages::rebase_attachments(tx, &from, &to)?;
            Ok(())
        })
        .await
        .unwrap();
    let messages = manager
        .agent_messages(&SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    let [moved] = messages[0].attachments.as_slice() else {
        panic!("{messages:?}");
    };
    assert_eq!(
        *moved,
        destination.join(copy.strip_prefix(&source).unwrap())
    );

    // Removed with nothing below it, the chat hands the files to the user.
    manager.end_executions(&chat.id).await;
    manager.assign_parent(&worker.id, None).await.unwrap();
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    let notifications = manager.notifications(false, 100).await.unwrap();
    let [notification] = notifications.as_slice() else {
        panic!("{notifications:?}");
    };
    assert_eq!(notification.message, "Which layout?");
    let [file] = notification.files.as_slice() else {
        panic!("{notification:?}");
    };
    assert_eq!(file.path, *moved);
    assert_eq!(std::fs::read_to_string(&file.path).unwrap(), "png");
}

#[tokio::test]
async fn same_named_spaces_reach_their_own_coordinators() {
    let (temp, manager) = manager().await;
    let (chat, worker) = chat_and_worker(&temp, &manager).await;
    let other_chat = manager
        .start_chat(
            worker.repository_id.clone().into(),
            "chat-2".into(),
            "codex".into(),
            None,
        )
        .await
        .unwrap();
    let other = manager
        .create_workspace(&worker.repository_id, "other".into(), None, None, None)
        .await
        .unwrap();
    manager
        .assign_parent(&other.id, Some(other_chat.id.clone()))
        .await
        .unwrap();
    // Both take the same name, which identifies neither.
    for workspace in [&worker, &other] {
        let selector = SpaceSelector::id(&workspace.id);
        (manager.set_workspace_name(&selector, &"Same work".parse().unwrap()))
            .await
            .unwrap();
        (manager.set_agent_state(&selector, AgentState::Working, None, None))
            .await
            .unwrap();
    }
    for (workspace, message) in [(&worker, "from the worker"), (&other, "from the other")] {
        manager
            .send_message(
                &SpaceSelector::id(&workspace.id),
                message.into(),
                Vec::new(),
                &Attacher::User,
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
        .create_workspace(&worker.repository_id, "sibling".into(), None, None, None)
        .await
        .unwrap();
    manager
        .assign_parent(&sibling.id, Some(chat.id.clone()))
        .await
        .unwrap();
    for workspace in [&worker, &sibling] {
        let selector = SpaceSelector::id(&workspace.id);
        (manager.set_workspace_name(&selector, &"Same work".parse().unwrap()))
            .await
            .unwrap();
        (manager.set_agent_state(&selector, AgentState::Working, None, None))
            .await
            .unwrap();
    }
    for workspace in [&worker, &sibling] {
        manager
            .send_message(
                &SpaceSelector::id(&workspace.id),
                "tests pass".into(),
                Vec::new(),
                &Attacher::User,
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
