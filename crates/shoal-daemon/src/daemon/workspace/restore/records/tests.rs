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
