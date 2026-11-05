use crate::{
    daemon::chats::{
        restores::tests::{RESUMED, UNRESUMABLE},
        saved::tests::named_chat,
    },
    state::ChatRestore,
    test_support::manager,
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

    manager.restart_chat(&chat.id, run("a")).await.unwrap();

    // Its agent stopped, and the daemon ran `shoal resume` for the saved chat.
    assert!(!process.wait().unwrap().success());
    let saved = manager.recorded_chat(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.restore, Some(ChatRestore::Resume));
    assert!(manager.chat_saved(&chat.id).await.unwrap());
    assert!(RESUMED.lock().unwrap().contains(&chat.id));
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

    let error = manager.restart_chat(&chat.id, run("a")).await.unwrap_err();

    assert!(
        format!("{error:#}").contains("shoal resume unrestored"),
        "{error:#}"
    );
    assert!(!process.wait().unwrap().success());
    let saved = manager.recorded_chat(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.restore, Some(ChatRestore::Resume));
    let agents = manager.running_chat_agents().await.unwrap();
    assert!(agents.iter().all(|agent| agent.execution != chat.id));
}
