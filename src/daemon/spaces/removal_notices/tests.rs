use super::*;
use crate::daemon::{
    events::{EventItem, EventKind},
    test_support::{manager, repository},
};

/// The causes of the retentions the journal records.
async fn retentions(manager: &Manager) -> Vec<Option<EventCause>> {
    let events = manager.workspace_events(None, 100).await.unwrap();
    events
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == EventKind::Retained => {
                Some(event.details.cause)
            }
            _ => None,
        })
        .collect()
}

async fn notices(manager: &Manager) -> Vec<(NotificationKind, Option<String>)> {
    let notifications = manager.notifications(false, 100).await.unwrap();
    notifications
        .into_iter()
        .map(|notification| (notification.kind, notification.workspace))
        .collect()
}

#[tokio::test]
async fn a_retained_workspace_records_its_cleanup_as_the_cause() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();

    let removal: Result<bool> = Err(anyhow::anyhow!("dirty worktree"));
    (manager.announce_removal(&workspace, Cleanup::Merged, &removal))
        .await
        .unwrap();

    assert_eq!(retentions(&manager).await, [Some(EventCause::Merged)]);
    assert_eq!(
        notices(&manager).await,
        [(NotificationKind::CleanupFailed, Some("topic".to_owned()))]
    );
}

#[tokio::test]
async fn a_retained_chat_records_no_retention() {
    let (_root, manager) = manager().await;
    let chat = Announced {
        kind: SpaceKind::Chat,
        id: &SpaceId::from("chat-id"),
        name: "notes",
    };

    let removal: Result<bool> = Err(anyhow::anyhow!("agent running"));
    (manager.announce_removal(chat, Cleanup::Idle, &removal))
        .await
        .unwrap();

    assert!(retentions(&manager).await.is_empty());
    assert_eq!(
        notices(&manager).await,
        [(NotificationKind::CleanupFailed, Some("notes".to_owned()))]
    );
}

#[test]
fn only_a_workspace_with_a_checkpoint_is_restorable() {
    let id = SpaceId::from("id");
    let space = |kind| Announced {
        kind,
        id: &id,
        name: "topic",
    };
    let restorable =
        |kind, cleanup| (space(kind).removal_notice(cleanup)).contains("shoal work restore topic");
    assert!(restorable(SpaceKind::Workspace, Cleanup::Idle));
    assert!(restorable(SpaceKind::Workspace, Cleanup::Parent("parent")));
    assert!(!restorable(SpaceKind::Workspace, Cleanup::DeletedWorktree));
    assert!(!restorable(SpaceKind::Chat, Cleanup::Parent("parent")));
}
