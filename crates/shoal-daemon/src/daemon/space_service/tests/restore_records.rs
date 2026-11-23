use crate::model::EventCause;
use crate::time::UnixTime;

use crate::daemon::test_support::{chat, manager, repository};
use crate::removal::{InspectionPolicy, UnsavedWork};
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn a_removed_chat_keeps_a_record_of_how_and_where_it_ran() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let removed_chat = chat(&manager, "notes", "plan", Some(&repo), Some(&parent.id)).await;
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&removed_chat.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();

    let removed = manager.spaces.removed_spaces().await.unwrap();
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
    assert_eq!(kept.mode.as_str(), "plan");
    assert_eq!(kept.workspace, None);
    // A chat is no workspace to open, export or copy.
    assert!(
        manager
            .spaces
            .removed_workspaces()
            .await
            .unwrap()
            .is_empty()
    );
    let chat_column: bool = (manager.store)
        .run(|db| Ok(db.query_row("SELECT chat FROM removed_spaces", [], |row| row.get(0))?))
        .await
        .unwrap();
    assert!(chat_column);
}

#[tokio::test]
async fn a_chat_a_conversion_removed_restores_in_the_workspace_restored_beside() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let resident = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "review".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let converted = crate::daemon::conversions::record::ConvertedChat {
        agent: "claude".into(),
        execution: shoal_ids::execution_ids::ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    (manager
        .spaces
        .convert_worktree_to_chat(&*manager, &workspace.id, &converted))
    .await
    .unwrap();

    let removed = manager.spaces.removed_workspaces().await.unwrap();
    let [record] = removed.as_slice() else {
        panic!("{removed:?}");
    };
    manager
        .spaces
        .restore_space(&*manager, &SpaceSelector::id(&record.space_id), None)
        .await
        .unwrap();
    let restored =
        (manager
            .spaces
            .restore_space(&*manager, &SpaceSelector::id(&resident.id), None))
        .await
        .unwrap();
    let crate::model::RestoredSpace::Chat(restored) = restored else {
        panic!("{restored:?}");
    };
    let place = restored
        .chat
        .workdir
        .workspace()
        .map(|workspace| workspace.id.clone());
    assert_eq!(place.as_ref(), Some(&record.space_id));
}
