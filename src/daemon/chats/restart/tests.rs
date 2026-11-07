use crate::{
    daemon::{
        chats::{
            restores::tests::{HELD, RESUMED, UNRESUMABLE},
            saved::tests::{named_chat, unmarked},
        },
        test_support::{ProcessGroup, manager, repository},
    },
    state::ChatRestore,
};

fn run(name: &str) -> Option<String> {
    Some(name.to_owned())
}

#[tokio::test]
async fn a_restart_saves_a_chat_and_restores_it_as_shoal_resume_does() {
    let (root, manager) = manager().await;
    let (_repo, chat, mut process) = named_chat(root.path(), &manager, "restarted").await;
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();

    manager
        .restart_chat(&chat.id, run("a"), None)
        .await
        .unwrap();

    // Its agent stopped, and the daemon ran `shoal resume` for the saved chat.
    assert!(!process.wait().unwrap().success());
    let saved = manager.recorded_chat(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.restore, Some(ChatRestore::Resume));
    assert!(manager.chat_saved(&chat.id).await.unwrap());
    assert!(RESUMED.lock().unwrap().contains(&chat.id));
    // The user's restart is the user's stop.
    assert_eq!(manager.chat_stopped_by(&chat.id).await.unwrap(), None);
    // A run that continues it learns the restart started it again.
    manager
        .join_chat_restart(&chat.id, run("a"), run("b"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_chat_whose_restore_fails_stays_stopped_for_shoal_resume() {
    let (root, manager) = manager().await;
    let (_repo, chat, mut process) = named_chat(root.path(), &manager, "unrestored").await;
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();
    UNRESUMABLE.lock().unwrap().push(chat.name.clone());

    let coordinator = Some("coordinator".to_owned());
    let error = (manager.restart_chat(&chat.id, run("a"), coordinator.clone()))
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("shoal resume unrestored"),
        "{error:#}"
    );
    assert!(!process.wait().unwrap().success());
    let saved = manager.recorded_chat(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.restore, Some(ChatRestore::Resume));
    // The space whose process asked for the restart stopped it.
    let stopper = manager.chat_stopped_by(&chat.id).await.unwrap();
    assert_eq!(stopper, coordinator);
    let agents = manager.running_chat_agents().await.unwrap();
    assert!(agents.iter().all(|agent| agent.execution != chat.id));
}

/// A chat a coordinator stopped is the user's to resume once its restored
/// agent exits on its own.
#[tokio::test]
async fn an_exit_after_a_restore_leaves_the_chat_to_the_user() {
    let (root, manager) = manager().await;
    let (_repo, chat, mut process) = named_chat(root.path(), &manager, "exited").await;
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();
    UNRESUMABLE.lock().unwrap().push(chat.name.clone());
    let coordinator = Some("coordinator".to_owned());
    let restart = manager.restart_chat(&chat.id, run("a"), coordinator);
    assert!(restart.await.is_err());
    process.wait().unwrap();
    let restored = ProcessGroup::spawn(unmarked("sleep").arg("600"));
    manager
        .restore_chat(&chat.id, restored.id(), None, false)
        .await
        .unwrap();

    restored.kill();

    let stopped = manager.chats().await.unwrap();
    assert_eq!(stopped[0].restore, Some(ChatRestore::Resume));
    assert_eq!(manager.chat_stopped_by(&chat.id).await.unwrap(), None);
}

/// A restore that fails leaves the chat stopped by whoever restored it,
/// whoever stopped it before.
#[tokio::test]
async fn a_failed_restore_stops_the_chat_for_its_restorer() {
    let (root, manager) = manager().await;
    let (_repo, chat, mut process) = named_chat(root.path(), &manager, "retried").await;
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();
    UNRESUMABLE.lock().unwrap().push(chat.name.clone());
    let coordinator = Some("coordinator");
    let restart = manager.restart_chat(&chat.id, run("a"), coordinator.map(str::to_owned));
    assert!(restart.await.is_err());
    process.wait().unwrap();
    let fail_restore = |stopped_by| {
        let manager = &manager;
        let id = &chat.id;
        async move {
            let restoring = ProcessGroup::spawn(unmarked("sleep").arg("600"));
            let pid = restoring.id();
            manager.restore_chat(id, pid, None, false).await.unwrap();
            (manager.return_to_saved(id, pid, "failed", stopped_by))
                .await
                .unwrap();
            restoring.kill();
            manager.chat_stopped_by(id).await.unwrap()
        }
    };

    assert_eq!(fail_restore(None).await, None);
    assert_eq!(fail_restore(coordinator).await.as_deref(), coordinator);
}

#[tokio::test]
async fn a_chat_whose_agent_cannot_resume_its_session_keeps_running() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // No AI tool describes the agent, so nothing resumes its session.
    let process = ProcessGroup::spawn(unmarked("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repo.id.into(),
            "plain".into(),
            "plain".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();

    let agents = manager.running_chat_agents().await.unwrap();
    assert!(!agents[0].restorable);
    assert!(
        manager
            .restart_chat(&chat.id, run("a"), None)
            .await
            .is_err()
    );
    let running = manager.recorded_chat(&chat.id).await.unwrap().unwrap();
    assert_eq!(running.restore, None);
    assert!(!RESUMED.lock().unwrap().contains(&chat.id));
}

#[tokio::test]
async fn a_daemon_update_waits_for_a_chat_restart() {
    let (root, manager) = manager().await;
    let (_repo, chat, _process) = named_chat(root.path(), &manager, "held").await;
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();
    HELD.lock().unwrap().push(chat.name.clone());
    let restart = tokio::spawn({
        let (manager, id) = (manager.clone(), chat.id.clone());
        async move { manager.restart_chat(&id, run("a"), None).await }
    });
    while !RESUMED.lock().unwrap().contains(&chat.id) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Saved and not yet restored, the chat keeps an update from starting.
    assert!(manager.gates.try_quiesce().is_none());
    HELD.lock().unwrap().retain(|name| *name != chat.name);
    restart.await.unwrap().unwrap();
    assert!(manager.gates.try_quiesce().is_some());
}

#[tokio::test]
async fn a_chat_whose_settings_cannot_be_read_does_not_fail_the_listing() {
    let (root, manager) = manager().await;
    let (repo, chat, _process) = named_chat(root.path(), &manager, "unreadable").await;
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();
    std::fs::write(repo.path.join(".shoal.toml"), "agents = [").unwrap();

    let agents = manager.running_chat_agents().await.unwrap();
    let listed = agents.iter().find(|agent| agent.execution == chat.id);
    assert!(listed.is_some_and(|agent| !agent.restorable), "{agents:?}");
}
