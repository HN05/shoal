use std::{fs, path::Path, sync::Arc};

use crate::{
    daemon::workspace::Manager,
    process::identity,
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{manager, repository},
    transfer::{ExportedSpace, Import, Space},
};

/// A manager with a chat `name` about a registered repository, whose
/// process this test reaps once a signal ends it, so the chat ends with it.
async fn chat(name: &str) -> (tempfile::TempDir, Arc<Manager>, u32) {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut process = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let pid = process.id();
    std::thread::spawn(move || process.wait());
    manager
        .start_chat(
            repo.id.into(),
            name.into(),
            "claude".into(),
            pid,
            None,
            false,
        )
        .await
        .unwrap();
    (root, manager, pid)
}

async fn export(manager: &Manager, selector: &str, file: &Path) -> anyhow::Result<ExportedSpace> {
    manager
        .export_space(
            selector,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            file.to_owned(),
        )
        .await
}

#[tokio::test]
async fn a_chat_ends_into_a_package_that_continues_its_session() {
    let (root, manager, pid) = chat("plan-cache").await;
    let chat = manager.chats().await.unwrap().remove(0);
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();
    let file = root.path().join("plan-cache.shoal");

    let exported = export(&manager, "plan-cache", &file).await.unwrap();
    assert!(exported.removal.is_none());
    assert!(manager.chats().await.unwrap().is_empty());
    assert_eq!(identity::capture(pid).unwrap(), None);
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let Space::Chat(packaged) = &import.manifest.space else {
        panic!("not a chat: {:?}", import.manifest);
    };
    assert_eq!(
        (packaged.name.as_str(), packaged.agent.as_str()),
        ("plan-cache", "claude")
    );
    assert!(import.bundle.is_none());
    let sessions = import.manifest.sessions.as_ref().unwrap();
    assert_eq!(sessions.reported().collect::<Vec<_>>(), [("claude", "s1")]);
    assert_eq!(sessions.last_sessions[0].execution, chat.id);
}

#[tokio::test]
async fn a_chat_without_a_session_keeps_running() {
    let (root, manager, pid) = chat("talk").await;
    let file = root.path().join("talk.shoal");

    let error = export(&manager, "talk", &file).await.unwrap_err();
    assert!(
        error.to_string().contains("reported no session"),
        "{error:#}"
    );
    let chat = manager.chats().await.unwrap().remove(0);
    assert!(identity::capture(pid).unwrap().is_some());
    assert!(!file.exists());
    manager.end_chat(&chat.id).await.unwrap();
}
