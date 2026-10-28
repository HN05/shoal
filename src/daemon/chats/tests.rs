use super::*;
use crate::{
    daemon::events::{ChatEventKind, EventItem, EventKind},
    test_support::{manager, repository},
};

/// Chat starts and ends, and workspace chat changes, in journal order.
async fn chat_events(manager: &Manager) -> Vec<String> {
    manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Chat(event) => Some(format!("{} {}", event.kind, event.chat.name)),
            EventItem::Event(event) if event.details.kind == EventKind::ChatChanged => Some(
                format!("{} {:?}", event.details.name, event.details.chat.flatten()),
            ),
            EventItem::Event(_) | EventItem::Gap { .. } => None,
        })
        .collect()
}

#[tokio::test]
async fn chats_last_while_their_process_runs_and_own_the_workspaces_they_open() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let mut process = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let ending = manager
        .start_chat(
            repo.id.clone().into(),
            "ending".into(),
            "claude".into(),
            process.id(),
            None,
        )
        .await
        .unwrap();
    let running = manager
        .start_chat(
            repo.id.clone().into(),
            "running".into(),
            "codex".into(),
            std::process::id(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        (ending.name.as_str(), running.name.as_str()),
        ("ending", "running")
    );
    assert_eq!(
        manager.chats().await.unwrap(),
        [ending.clone(), running.clone()]
    );

    manager
        .assign_chat(&workspace.id, Some(ending.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        manager.workspace(&workspace.id).await.unwrap().chat,
        Some(ending.id.clone())
    );
    // An unknown chat gives the workspace to the user rather than failing.
    manager
        .assign_chat(&workspace.id, Some("gone".into()))
        .await
        .unwrap();
    assert_eq!(manager.workspace(&workspace.id).await.unwrap().chat, None);
    manager
        .assign_chat(&workspace.id, Some(ending.id.clone()))
        .await
        .unwrap();

    // The chat ends with its process, returning its workspace to the user,
    // and its name is free again.
    process.kill().unwrap();
    process.wait().unwrap();
    assert_eq!(manager.chats().await.unwrap(), [running]);
    assert_eq!(manager.workspace(&workspace.id).await.unwrap().chat, None);
    let next = manager
        .start_chat(
            repo.id.clone().into(),
            "ending".into(),
            "claude".into(),
            std::process::id(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(next.name, "ending");
    assert_eq!(
        chat_events(&manager).await,
        [
            format!("{} ending", ChatEventKind::Started),
            format!("{} running", ChatEventKind::Started),
            format!("worker {:?}", Some(&ending.id)),
            "worker None".to_owned(),
            format!("worker {:?}", Some(&ending.id)),
            "worker None".to_owned(),
            format!("{} ending", ChatEventKind::Ended),
            format!("{} ending", ChatEventKind::Started),
        ]
    );
}

#[tokio::test]
async fn a_chat_needs_a_running_process() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut process = std::process::Command::new("true").spawn().unwrap();
    let pid = process.id();
    process.wait().unwrap();
    let error = manager
        .start_chat(repo.id.into(), "chat-4".into(), "claude".into(), pid, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not running"), "{error:#}");
    assert!(manager.chats().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_moved_chat_lasts_while_its_new_process_runs() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.into(),
            "chat-5".into(),
            "happy-claude".into(),
            std::process::id(),
            None,
        )
        .await
        .unwrap();
    let mut process = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let moved = manager.move_chat(&chat.id, process.id()).await.unwrap();
    assert_eq!(
        moved,
        Chat {
            pid: process.id(),
            ..chat.clone()
        }
    );
    assert_eq!(manager.chats().await.unwrap(), [moved]);

    // The recording process still runs, but the chat ends with the new one.
    process.kill().unwrap();
    process.wait().unwrap();
    assert!(manager.chats().await.unwrap().is_empty());
    let error = manager
        .move_chat(&chat.id, std::process::id())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unknown chat"), "{error:#}");
}

#[tokio::test]
async fn a_chat_moved_after_its_process_was_found_ended_keeps_running() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut recording = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.into(),
            "chat-6".into(),
            "happy-claude".into(),
            recording.id(),
            None,
        )
        .await
        .unwrap();
    // A sweep finds the recording process ended...
    let recorded = manager.store.run(recorded_processes).await.unwrap();
    recording.kill().unwrap();
    recording.wait().unwrap();
    let ended = finished(recorded);
    assert_eq!(ended.len(), 1);
    // ...while the chat moves to its session's process.
    let moved = manager
        .move_chat(&chat.id, std::process::id())
        .await
        .unwrap();
    manager
        .store
        .run(move |db| delete(db, &ended))
        .await
        .unwrap();
    assert_eq!(manager.chats().await.unwrap(), [moved]);
}

#[tokio::test]
async fn chats_and_workspaces_share_one_namespace() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let start = |name: &str, pid| {
        manager.start_chat(
            repo.id.clone().into(),
            name.into(),
            "claude".into(),
            pid,
            None,
        )
    };
    let refusal = |result: Result<Chat>| format!("{:#}", result.unwrap_err());

    // A chat takes neither a workspace's name nor an invalid one.
    assert!(refusal(start("worker", std::process::id()).await).contains("workspace name"));
    assert!(refusal(start("-talk", std::process::id()).await).contains("chat names must"));
    let mut process = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    start("talk", process.id()).await.unwrap();
    assert!(
        refusal(start("talk", std::process::id()).await)
            .contains("a running chat already has the name talk")
    );
    assert!(manager.check_chat_name("talk".into()).await.is_err());
    manager.check_chat_name("other".into()).await.unwrap();
    assert_eq!(
        manager.free_chat_name("talk".into()).await.unwrap(),
        "talk-2"
    );
    assert_eq!(
        manager.free_chat_name("worker".into()).await.unwrap(),
        "worker-2"
    );
    assert_eq!(
        manager.free_chat_name("re po".into()).await.unwrap(),
        "re-po"
    );

    // A workspace is neither checked, created nor renamed to a running
    // chat's name.
    let options = Default::default();
    let error = manager
        .check_creation(&repo.id, Some("talk"), None, options)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("running chat"), "{error:#}");
    let error = manager
        .create_workspace(&repo.id, "talk".into(), None, None, None)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("running chat"), "{error:#}");
    let error = manager
        .rename_workspace(&worker.id, "talk", None)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("running chat"), "{error:#}");

    // The name frees once the chat's process ends, without listing chats.
    process.kill().unwrap();
    process.wait().unwrap();
    manager
        .check_creation(&repo.id, Some("talk"), None, options)
        .await
        .unwrap();
    let renamed = manager
        .rename_workspace(&worker.id, "talk", None)
        .await
        .unwrap();
    assert_eq!(renamed.name, "talk");
    assert!(refusal(start("talk", std::process::id()).await).contains("workspace name"));
}

#[test]
fn suffixed_names_fit_the_name_limit() {
    let long = "a".repeat(crate::validate::MAX_NAME_LEN);
    assert_eq!(suffixed("chat", 1), "chat");
    assert_eq!(suffixed("chat", 3), "chat-3");
    let cut = suffixed(&long, 12);
    assert_eq!(cut.len(), crate::validate::MAX_NAME_LEN);
    assert!(cut.ends_with("a-12"), "{cut}");
}
