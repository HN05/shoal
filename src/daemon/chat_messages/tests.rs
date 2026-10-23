use crate::{
    daemon::{
        notifications::NotificationKind,
        workspace::{Manager, Source},
    },
    model::{Chat, Workspace},
    state::AgentState,
    test_support::{manager, repository},
};

/// A repository with a workspace named `worker` and a chat whose process is
/// this test.
async fn chat_and_worker(temp: &tempfile::TempDir, manager: &Manager) -> (Chat, Workspace) {
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
        .start_chat(repo.id.into(), "claude".into(), std::process::id())
        .await
        .unwrap();
    manager
        .assign_chat(&worker.id, Some(chat.id.clone()))
        .await
        .unwrap();
    (chat, worker)
}

async fn received(manager: &Manager, chat: &Chat) -> Vec<String> {
    let messages = manager.chat_messages(&chat.id).await.unwrap();
    messages.into_iter().map(|m| m.message).collect()
}

async fn user(manager: &Manager) -> Vec<(Option<String>, NotificationKind, String)> {
    let notifications = manager.notifications(false, 100).await.unwrap();
    notifications
        .into_iter()
        .map(|n| (n.workspace, n.kind, n.message))
        .collect()
}

#[tokio::test]
async fn a_chat_coordinates_the_workspaces_it_owns_below_their_agents() {
    let (temp, manager) = manager().await;
    let (chat, worker) = chat_and_worker(&temp, &manager).await;
    let helper = manager
        .split_workspace(&worker.id, "helper".into(), None)
        .await
        .unwrap();
    // Without an agent of its own, the worker is the chat's or the user's
    // own work, which the user hears about.
    manager
        .send_message(&worker.id, "first".into())
        .await
        .unwrap();
    manager
        .set_agent_state(&worker.id, AgentState::Working, None, None)
        .await
        .unwrap();
    manager
        .set_agent_state(&helper.id, AgentState::Working, None, None)
        .await
        .unwrap();
    manager
        .send_message(&worker.id, "second".into())
        .await
        .unwrap();
    // The worker's agent coordinates the helper below it, not the chat.
    manager
        .send_message(&helper.id, "to the worker".into())
        .await
        .unwrap();
    assert_eq!(received(&manager, &chat).await, ["worker: second"]);
    let worker_messages = manager.agent_messages(&worker.id).await.unwrap();
    assert_eq!(worker_messages[0].message, "helper: to the worker");
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
            db.execute(
                "DELETE FROM workspace_agent_state WHERE workspace_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager
        .send_message(&helper.id, "to the chat".into())
        .await
        .unwrap();
    assert_eq!(
        received(&manager, &chat).await,
        ["worker: second", "helper: to the chat"]
    );
}

#[tokio::test]
async fn delivery_deletes_chat_messages_and_replaces_a_running_wait() {
    let (temp, manager) = manager().await;
    let (chat, worker) = chat_and_worker(&temp, &manager).await;
    manager
        .set_agent_state(&worker.id, AgentState::Working, None, None)
        .await
        .unwrap();

    let manager = std::sync::Arc::new(manager);
    let waiting = tokio::spawn({
        let (manager, chat) = (manager.clone(), chat.id.clone());
        async move { manager.wait_chat_messages(&chat).await }
    });
    manager
        .send_message(&worker.id, "done with the tests".into())
        .await
        .unwrap();
    let woken = waiting.await.unwrap().unwrap();
    assert!(!woken.superseded);
    assert_eq!(woken.messages[0].message, "worker: done with the tests");

    let ids = woken.messages.iter().map(|m| m.id).collect();
    manager
        .mark_chat_messages_delivered(&chat.id, ids)
        .await
        .unwrap();
    assert!(received(&manager, &chat).await.is_empty());

    // A hook delivery replaces a wait still running, which leaves the next
    // messages to the delivery.
    let waiting = tokio::spawn({
        let (manager, chat) = (manager.clone(), chat.id.clone());
        async move { manager.wait_chat_messages(&chat).await }
    });
    while !manager.chat_waits.is_waiting(&chat.id) {
        tokio::task::yield_now().await;
    }
    assert!(
        manager
            .chat_updates(&chat.id)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(waiting.await.unwrap().unwrap().superseded);
    manager
        .send_message(&worker.id, "next".into())
        .await
        .unwrap();
    let delivered = manager.chat_updates(&chat.id).await.unwrap();
    assert_eq!(delivered.messages[0].message, "worker: next");
}

#[tokio::test]
async fn an_ended_chat_gives_the_user_the_events_it_had_not_received() {
    let (temp, manager) = manager().await;
    let (_, worker) = chat_and_worker(&temp, &manager).await;
    let mut process = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let chat = manager
        .start_chat(
            worker.repository_id.clone().into(),
            "codex".into(),
            process.id(),
        )
        .await
        .unwrap();
    manager
        .assign_chat(&worker.id, Some(chat.id.clone()))
        .await
        .unwrap();
    manager
        .set_agent_state(&worker.id, AgentState::Working, None, None)
        .await
        .unwrap();
    manager
        .send_message(&worker.id, "unread".into())
        .await
        .unwrap();
    // Ready marks reach only a coordinator, never the user.
    assert!(
        manager
            .tell_coordinator(&worker.name, None, "marked ready".into(), Source::Agent)
            .await
    );
    assert_eq!(
        received(&manager, &chat).await,
        ["worker: unread", "worker: marked ready"]
    );
    assert!(user(&manager).await.is_empty());

    process.kill().unwrap();
    process.wait().unwrap();
    manager.end_finished_chats().await.unwrap();
    assert!(received(&manager, &chat).await.is_empty());
    let unread = (
        Some("worker".to_owned()),
        NotificationKind::AgentMessage,
        "unread".to_owned(),
    );
    assert_eq!(user(&manager).await, std::slice::from_ref(&unread));
    // The worker is the user's again.
    manager
        .send_message(&worker.id, "later".into())
        .await
        .unwrap();
    assert_eq!(
        user(&manager).await,
        [
            unread,
            (
                Some("worker".into()),
                NotificationKind::AgentMessage,
                "later".into()
            )
        ]
    );
}

#[tokio::test]
async fn an_ended_chat_keeps_who_sent_the_events_it_hands_over() {
    let (temp, manager) = manager().await;
    let (_, worker) = chat_and_worker(&temp, &manager).await;
    let mut process = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let chat = manager
        .start_chat(
            worker.repository_id.clone().into(),
            "codex".into(),
            process.id(),
        )
        .await
        .unwrap();
    manager
        .assign_chat(&worker.id, Some(chat.id.clone()))
        .await
        .unwrap();
    manager
        .set_agent_state(&worker.id, AgentState::Working, None, None)
        .await
        .unwrap();
    manager
        .send_message(&worker.id, "unread".into())
        .await
        .unwrap();
    // The worker is renamed and a new workspace takes its name before the
    // chat ends.
    manager
        .rename_workspace(&worker.id, "renamed", None)
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

    process.kill().unwrap();
    process.wait().unwrap();
    manager.end_finished_chats().await.unwrap();
    let handed = manager.notifications(true, 50).await.unwrap();
    assert_eq!(handed.len(), 1);
    let id = handed[0].id;
    let error = manager
        .withdraw_message(&successor.id, id)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("sent no notification"),
        "{error:#}"
    );
    manager.withdraw_message(&worker.id, id).await.unwrap();
}
