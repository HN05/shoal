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
            EventItem::Chat(event) => Some(format!("{} {}", event.kind, event.chat.number)),
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
        .start_chat(repo.id.clone().into(), "claude".into(), process.id())
        .await
        .unwrap();
    let running = manager
        .start_chat(repo.id.clone().into(), "codex".into(), std::process::id())
        .await
        .unwrap();
    assert_eq!((ending.number, running.number), (1, 2));
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
    // and its number is free again.
    process.kill().unwrap();
    process.wait().unwrap();
    assert_eq!(manager.chats().await.unwrap(), [running]);
    assert_eq!(manager.workspace(&workspace.id).await.unwrap().chat, None);
    let next = manager
        .start_chat(repo.id.clone().into(), "claude".into(), std::process::id())
        .await
        .unwrap();
    assert_eq!(next.number, 1);
    assert_eq!(
        chat_events(&manager).await,
        [
            format!("{} 1", ChatEventKind::Started),
            format!("{} 2", ChatEventKind::Started),
            format!("worker {:?}", Some(&ending.id)),
            "worker None".to_owned(),
            format!("worker {:?}", Some(&ending.id)),
            "worker None".to_owned(),
            format!("{} 1", ChatEventKind::Ended),
            format!("{} 1", ChatEventKind::Started),
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
        .start_chat(repo.id.into(), "claude".into(), pid)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not running"), "{error:#}");
    assert!(manager.chats().await.unwrap().is_empty());
}
