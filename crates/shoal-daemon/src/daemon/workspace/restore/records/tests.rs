use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::test_support::{chat, manager, repository};

#[tokio::test]
async fn a_removed_chat_keeps_a_record_of_how_and_where_it_ran() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let removed_chat = chat(&manager, "notes", "plan", Some(&repo), Some(&parent.id)).await;
    manager
        .remove_chat(&SpaceSelector::id(&removed_chat.id), None)
        .await
        .unwrap();

    let removed = manager.removed_spaces().await.unwrap();
    let [record] = removed.as_slice() else {
        panic!("{removed:?}");
    };
    assert_eq!(record.space_id, removed_chat.id);
    assert_eq!(record.repository_id, Some(repo.id.clone()));
    assert_eq!(
        record.parent.as_ref().map(|parent| &parent.id),
        Some(&parent.id)
    );
    assert_eq!(record.cause, EventCause::Manual);
    assert!(record.expires_at > UnixTime::now());
    let kept = record.chat().expect("a chat's record");
    assert_eq!(
        (kept.agent.to_string(), kept.mode.to_string()),
        ("claude".into(), "plan".into())
    );
    assert_eq!(kept.workspace, None);
    // A chat is no workspace to open, export or copy.
    assert!(manager.removed_workspaces().await.unwrap().is_empty());
    let chat_column: bool = (manager.store)
        .run(|db| Ok(db.query_row("SELECT chat FROM removed_spaces", [], |row| row.get(0))?))
        .await
        .unwrap();
    assert!(chat_column);
}

#[tokio::test]
async fn a_converted_workspace_keeps_its_record_beside_the_chat_it_became() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let converted = crate::daemon::conversions::record::ConvertedChat {
        agent: "claude".into(),
        execution: crate::client::execution_ids::ExecutionId::generate(),
        session: None,
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    (manager.convert_worktree_to_chat(&workspace.id, &converted))
        .await
        .unwrap();
    let chat = SpaceSelector::id(&workspace.id);
    manager.remove_chat(&chat, None).await.unwrap();

    // Each record has its own ID: the chat the workspace's, the workspace
    // a new one, which its checkpoints moved to.
    let removed = manager.removed_spaces().await.unwrap();
    let ids = |kind: fn(&RemovedSpace) -> bool| -> Vec<SpaceId> {
        (removed.iter().filter(|removed| kind(removed)))
            .map(|removed| removed.space_id.clone())
            .collect()
    };
    assert_eq!(
        ids(|removed| removed.chat().is_some()),
        std::slice::from_ref(&workspace.id)
    );
    let workspaces = ids(|removed| removed.workspace().is_some());
    let [record] = workspaces.as_slice() else {
        panic!("{removed:?}");
    };
    let record = record.clone();
    let owner = record.clone();
    let checkpoints: i64 = (manager.store)
        .run(move |db| {
            Ok(db.query_row(
                "SELECT COUNT(*) FROM checkpoints WHERE workspace_id=?1",
                [&owner],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(checkpoints, 1);

    let restored = manager.restore_space(&chat, None).await.unwrap();
    assert!(matches!(restored, crate::model::RestoredSpace::Chat(_)));
    // The workspace restores beside the chat, under its record's ID.
    let restored = (manager.restore_space(&SpaceSelector::id(&record), None))
        .await
        .unwrap();
    let crate::model::RestoredSpace::Workspace(restored) = restored else {
        panic!("{restored:?}");
    };
    assert_eq!(restored.opened.workspace.id, record);
    assert!(manager.removed_spaces().await.unwrap().is_empty());
}
