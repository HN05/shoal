use std::sync::Arc;

use super::*;
use crate::daemon::test_support::{chat, manager, repository};
use crate::model::{RestoredSpace, Workspace};
use crate::removal::{InspectionPolicy, UnsavedWork};

/// A manager with a registered repository and a workspace in it.
async fn with_workspace() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

/// Restore the removed space `id`, which must be a chat.
async fn restore_chat(manager: &Manager, id: &crate::client::space_ids::SpaceId) -> RestoredChat {
    match manager.restore_space(&SpaceSelector::id(id), None).await {
        Ok(RestoredSpace::Chat(restored)) => *restored,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_removed_chat_comes_back_under_its_id_below_its_parent() {
    let (_root, manager, parent) = with_workspace().await;
    let repo = manager.repository(&parent.repository_id).await.unwrap();
    let removed = chat(&manager, "notes", "plan", Some(&repo), Some(&parent.id)).await;
    let selector = SpaceSelector::id(&removed.id);
    manager.remove_chat(&selector, None).await.unwrap();

    let restored = restore_chat(&manager, &removed.id).await;
    let chat = restored.chat;
    assert_eq!(chat.id, removed.id);
    assert_eq!(chat.name, removed.name);
    assert_eq!(chat.mode, removed.mode);
    assert!(!chat.running);
    assert_eq!(chat.parent.map(|parent| parent.id), Some(parent.id));
    assert!(manager.removed_spaces().await.unwrap().is_empty());
    let id = chat.id.clone();
    let last: String = (manager.store)
        .run(move |db| {
            Ok(db.query_row(
                "SELECT json_extract(record,'$.kind') FROM workspace_events
                    WHERE space_id=?1 ORDER BY id DESC LIMIT 1",
                [&id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(last, "restored");
}

#[tokio::test]
async fn a_chat_in_a_removed_workspace_restores_once_that_workspace_does() {
    let (_root, manager, workspace) = with_workspace().await;
    let selector = SpaceSelector::id(&workspace.id);
    let removed = manager
        .start_chat_in_workspace(&selector, "review".into(), "claude".into(), None)
        .await
        .unwrap();
    (manager.remove_workspace(&selector, UnsavedWork::All, InspectionPolicy::GitOnly))
        .await
        .unwrap();
    assert_eq!(manager.removed_spaces().await.unwrap().len(), 2);

    let refused = (manager.restore_space(&SpaceSelector::id(&removed.id), None))
        .await
        .unwrap_err();
    let command = format!("shoal restore {} {}", workspace.id, removed.id);
    assert!(format!("{refused:#}").contains(&command), "{refused:#}");

    manager.restore_workspace(&selector).await.unwrap();
    let restored = restore_chat(&manager, &removed.id).await;
    let place = restored.chat.workspace.map(|workspace| workspace.id);
    assert_eq!(place, Some(workspace.id));
}
