use std::sync::{Arc, Mutex};

use super::restore_automatic;
use crate::{
    daemon::{
        chats::saved::tests::{named_chat, running_chat},
        notifications::NotificationKind,
        workspace::Manager,
    },
    model::Chat,
    state::ChatRestore,
    test_support::manager,
};

/// The chats the daemon ran `shoal resume` for, by ID.
pub(in crate::daemon::chats) static RESUMED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// The chats, by name, whose `shoal resume` fails.
pub(in crate::daemon::chats) static UNRESUMABLE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Restore the chats saved to come back on their own and wait for their
/// restores; returns how many started.
async fn restore_saved(manager: &Arc<Manager>) -> usize {
    let restores = restore_automatic(manager).await.unwrap();
    let started = restores.len();
    for restore in restores {
        restore.await.unwrap();
    }
    started
}

fn resumed(chat: &Chat) -> usize {
    RESUMED
        .lock()
        .unwrap()
        .iter()
        .filter(|id| **id == chat.id)
        .count()
}

#[tokio::test]
async fn a_chat_saved_to_come_back_restores_once_agents_may_start() {
    let (root, manager) = manager().await;
    let (_repo, chat, mut process) = running_chat(root.path(), &manager).await;
    manager.save_chats(ChatRestore::Automatic).await;
    process.wait().unwrap();

    // Not before a shutdown under way is cancelled.
    manager.agent_queue.suspend(true).await;
    assert_eq!(restore_saved(&manager).await, 0);
    let saved = manager.recorded_chat(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.restore, Some(ChatRestore::Automatic));

    manager.agent_queue.suspend(false).await;
    assert_eq!(restore_saved(&manager).await, 1);
    assert_eq!(resumed(&chat), 1);
    // The restore it started takes it over; until then it waits for
    // `shoal resume`, so no sweep starts another.
    let saved = manager.recorded_chat(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.restore, Some(ChatRestore::Resume));
    assert_eq!(restore_saved(&manager).await, 0);
    assert_eq!(resumed(&chat), 1);
}

#[tokio::test]
async fn a_failed_restore_tells_the_user_and_waits_for_resume() {
    let (root, manager) = manager().await;
    let (_repo, chat, mut process) = named_chat(root.path(), &manager, "unreachable").await;
    UNRESUMABLE.lock().unwrap().push(chat.name.clone());
    manager.save_chats(ChatRestore::Automatic).await;
    process.wait().unwrap();

    assert_eq!(restore_saved(&manager).await, 1);

    let notifications = manager.notifications(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == NotificationKind::ChatRestoreFailed),
        "{notifications:?}"
    );
    let saved = manager.recorded_chat(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.restore, Some(ChatRestore::Resume));
}

#[tokio::test]
async fn a_saved_chat_whose_process_runs_on_is_running() {
    let (root, manager) = manager().await;
    let (_repo, chat, _process) = running_chat(root.path(), &manager).await;
    // As a save whose stop failed after the mark left it.
    manager
        .store
        .run(|db| {
            db.execute("UPDATE chats SET restore='automatic'", [])?;
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(restore_saved(&manager).await, 0);

    assert!(manager.running_chat(&chat.id).await.unwrap().is_some());
    assert_eq!(resumed(&chat), 0);
}

#[tokio::test]
async fn a_restore_sweep_during_a_save_waits_and_sees_the_shutdown() {
    let (root, manager) = manager().await;
    let (_repo, chat, _process) = running_chat(root.path(), &manager).await;
    // A shutdown's save under way: its agent still runs.
    let saving = manager.chat_saves.lock().await;
    manager
        .store
        .run(|db| {
            db.execute("UPDATE chats SET restore='automatic'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let sweep = tokio::spawn({
        let manager = manager.clone();
        async move { restore_automatic(&manager).await.unwrap().len() }
    });
    tokio::task::yield_now().await;
    manager.agent_queue.suspend(true).await;
    drop(saving);

    assert_eq!(sweep.await.unwrap(), 0);
    let saved = manager.recorded_chat(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.restore, Some(ChatRestore::Automatic));
}
