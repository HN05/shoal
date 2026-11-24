use crate::daemon::manager::Manager;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

use crate::daemon::test_support::{chat, manager, repository};
use crate::model::{RestoredChat, RestoredSpace, Workspace};
use crate::removal::{InspectionPolicy, UnsavedWork};

/// A manager with a registered repository and a workspace in it.
async fn with_workspace() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

/// Restore the removed space `id`, which must be a chat.
async fn restore_chat(manager: &Manager, id: &shoal_ids::space_ids::SpaceId) -> RestoredChat {
    match manager
        .spaces
        .restore_space(manager, &SpaceSelector::id(id), None)
        .await
    {
        Ok(RestoredSpace::Chat(restored)) => *restored,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_removed_chat_comes_back_under_its_id_below_its_parent() {
    let (_root, manager, parent) = with_workspace().await;
    let repo = manager
        .repositories
        .repository(parent.repository())
        .await
        .unwrap();
    let removed = chat(&manager, "notes", "plan", Some(&repo), Some(&parent.id)).await;
    let selector = SpaceSelector::id(&removed.id);
    manager
        .spaces
        .remove_selected(
            &*manager,
            &selector,
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();

    let restored = restore_chat(&manager, &removed.id).await;
    let chat = restored.chat;
    assert_eq!(chat.id, removed.id);
    assert_eq!(chat.name, removed.name);
    assert_eq!(chat.mode, removed.mode);
    assert!(!chat.running);
    assert_eq!(chat.parent.map(|parent| parent.id), Some(parent.id));
    assert!(manager.spaces.removed_spaces().await.unwrap().is_empty());
    let id = chat.id.clone();
    let last: String = (manager.store)
        .run(move |db| {
            Ok(db.query_row(
                "SELECT json_extract(record,'$.kind') FROM space_events
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
async fn a_removed_chat_keeps_its_running_agent_with_its_session() {
    let (_root, manager, parent) = with_workspace().await;
    let repo = manager
        .repositories
        .repository(parent.repository())
        .await
        .unwrap();
    let removed = chat(&manager, "notes", "plan", Some(&repo), None).await;
    crate::daemon::test_support::report_session(&manager, &removed.id, "s1").await;
    let selector = SpaceSelector::id(&removed.id);
    manager
        .spaces
        .remove_selected(
            &*manager,
            &selector,
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();

    let records = manager.spaces.removed_spaces().await.unwrap();
    let [record] = records.as_slice() else {
        panic!("{records:?}");
    };
    let sessions: Vec<_> = (record.agents.iter())
        .map(|agent| {
            agent
                .record
                .session
                .as_ref()
                .map(|session| session.as_str())
        })
        .collect();
    assert_eq!(sessions, [Some("s1")]);
    let restored = restore_chat(&manager, &removed.id).await;
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
}
