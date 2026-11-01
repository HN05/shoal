use crate::{
    daemon::{
        events::EventItem,
        workspace::{Ending, Manager},
    },
    model::{Chat, Repository},
    state::ChatRestore,
    test_support::{ProcessGroup, manager, repository},
};

/// A registered repository and the chat `talk` running in it as a process
/// that lives until stopped.
pub(in crate::daemon::chats) async fn running_chat(
    root: &std::path::Path,
    manager: &Manager,
) -> (Repository, Chat, ProcessGroup) {
    named_chat(root, manager, "talk").await
}

/// [`running_chat`] named `name`.
pub(in crate::daemon::chats) async fn named_chat(
    root: &std::path::Path,
    manager: &Manager,
    name: &str,
) -> (Repository, Chat, ProcessGroup) {
    let checkout = repository(root, "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            name.into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    (repo, chat, process)
}

/// The chat events of the journal, as `<kind> <name>`.
async fn chat_events(manager: &Manager) -> Vec<String> {
    manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Chat(event) => Some(format!("{} {}", event.kind, event.chat.name)),
            EventItem::Event(_) | EventItem::File(_) | EventItem::Gap { .. } => None,
        })
        .collect()
}

#[tokio::test]
async fn a_saved_chat_keeps_its_record_until_it_is_restored() {
    let (root, manager) = manager().await;
    let (repo, chat, mut process) = running_chat(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    manager
        .assign_chat(&workspace.id, Some(chat.id.clone()))
        .await
        .unwrap();
    let tmp_dir = chat.tmp_dir.clone().unwrap();
    std::fs::write(tmp_dir.join("notes"), "kept").unwrap();

    manager.save_chats(ChatRestore::Automatic).await;

    assert!(!process.wait().unwrap().success());
    let saved = manager.chats().await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].restore, Some(ChatRestore::Automatic));
    assert!(manager.running_chat("talk").await.unwrap().is_none());
    assert_eq!(
        manager.workspace(&workspace.id).await.unwrap().chat,
        Some(chat.id.clone())
    );
    assert!(manager.check_chat_name("talk".into()).await.is_err());
    assert_eq!(
        std::fs::read_to_string(tmp_dir.join("notes")).unwrap(),
        "kept"
    );

    let restored = manager
        .restore_chat(&chat.id, std::process::id(), None, false)
        .await
        .unwrap();
    assert_eq!(
        (
            restored.id.as_str(),
            restored.name.as_str(),
            restored.restore
        ),
        (chat.id.as_str(), "talk", None)
    );
    assert_eq!(restored.tmp_dir.as_deref(), Some(tmp_dir.as_path()));
    assert_eq!(manager.chats().await.unwrap(), [restored]);
    assert_eq!(
        std::fs::read_to_string(tmp_dir.join("notes")).unwrap(),
        "kept"
    );
    let error = manager
        .restore_chat(&chat.id, std::process::id(), None, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no saved chat"), "{error:#}");
    assert_eq!(
        chat_events(&manager).await,
        ["chat_started talk", "chat_saved talk", "chat_restored talk"]
    );
}

#[tokio::test]
async fn a_discarded_saved_chat_ends() {
    let (root, manager) = manager().await;
    let (_repo, chat, _process) = running_chat(root.path(), &manager).await;
    let error = manager.discard_saved_chat("talk").await.unwrap_err();
    assert!(error.to_string().contains("no saved chat"), "{error:#}");

    manager.save_chats(ChatRestore::Resume).await;
    let discarded = manager.discard_saved_chat("talk").await.unwrap();

    assert_eq!(discarded.id, chat.id);
    assert!(manager.chats().await.unwrap().is_empty());
    assert!(!chat.tmp_dir.unwrap().exists());
    manager.check_chat_name("talk".into()).await.unwrap();
    assert_eq!(
        chat_events(&manager).await,
        ["chat_started talk", "chat_saved talk", "chat_ended talk"]
    );
}

#[tokio::test]
async fn the_daemon_saves_running_chats_as_it_exits() {
    for (ending, restore) in [
        (Ending::Stop, ChatRestore::Resume),
        (Ending::Save, ChatRestore::Automatic),
    ] {
        let (root, manager) = manager().await;
        let (_repo, chat, mut process) = running_chat(root.path(), &manager).await;

        manager.end_work(ending).await;

        let saved = manager.recorded_chat(&chat.id).await.unwrap().unwrap();
        assert_eq!(saved.restore, Some(restore), "{ending:?}");
        assert!(!process.wait().unwrap().success());
    }
}

#[tokio::test]
async fn a_detaching_daemon_leaves_chats_running() {
    let (root, manager) = manager().await;
    let (_repo, chat, _process) = running_chat(root.path(), &manager).await;

    manager.end_work(Ending::Detach).await;

    assert!(manager.running_chat(&chat.id).await.unwrap().is_some());
}

#[tokio::test]
async fn a_saved_chat_whose_process_runs_on_is_not_restored() {
    let (root, manager) = manager().await;
    let (_repo, chat, _process) = running_chat(root.path(), &manager).await;
    // As a save whose stop never ended the agent left it.
    manager
        .store
        .run(|db| {
            db.execute("UPDATE chats SET restore='resume'", [])?;
            Ok(())
        })
        .await
        .unwrap();

    let error = manager
        .restore_chat(&chat.id, std::process::id(), None, false)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("still runs"), "{error:#}");
    let saved = manager.recorded_chat(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.pid, chat.pid);
}

#[tokio::test]
async fn a_restore_whose_agent_did_not_start_waits_for_resume_again() {
    let (root, manager) = manager().await;
    let (_repo, chat, mut process) = running_chat(root.path(), &manager).await;
    manager.save_chats(ChatRestore::Automatic).await;
    process.wait().unwrap();
    let restoring = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let restoring_pid = restoring.id();
    manager
        .restore_chat(&chat.id, restoring_pid, None, false)
        .await
        .unwrap();

    manager
        .return_to_saved(&chat.id, restoring_pid)
        .await
        .unwrap();
    restoring.kill();

    let saved = manager.chats().await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].restore, Some(ChatRestore::Resume));
    assert!(chat.tmp_dir.unwrap().exists());
    // Another process cannot return it.
    manager
        .restore_chat(&chat.id, std::process::id(), None, false)
        .await
        .unwrap();
    let error = manager.return_to_saved(&chat.id, 1).await.unwrap_err();
    assert!(error.to_string().contains("does not run"), "{error:#}");
}
