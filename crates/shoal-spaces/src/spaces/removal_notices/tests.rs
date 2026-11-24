use crate::model::EventCause;
use crate::model::SpaceKind;
use crate::notifications::AlertKind;
use crate::spaces::removal_notices::*;
use crate::stand_in::Fixture;
use anyhow::Result;
use shoal_core::test_support::repository;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;

use crate::events::{EventItem, EventKind};

/// The causes of the retentions the journal records.
async fn retentions(manager: &Fixture) -> Vec<Option<EventCause>> {
    let events = manager.space_events(None, 100).await.unwrap();
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

async fn notices(manager: &Fixture) -> Vec<(AlertKind, Option<String>)> {
    let notifications = manager.spaces.notifications.list(false, 100).await.unwrap();
    notifications
        .into_iter()
        .map(|notification| (notification.kind, notification.space.map(String::from)))
        .collect()
}

#[tokio::test]
async fn a_retained_chat_records_its_cleanup_as_the_cause() {
    let manager = Fixture::new().await;
    let chat = manager.chat("notes", "chat", None, None).await;

    let removal: Result<bool> = Err(anyhow::anyhow!("agent running"));
    let space: crate::model::Space = chat.into();
    (manager
        .spaces
        .announce_removal(&*manager, &space, Cleanup::Idle, &removal))
    .await
    .unwrap();

    assert_eq!(retentions(&manager).await, [Some(EventCause::Idle)]);
    assert_eq!(
        notices(&manager).await,
        [(AlertKind::CleanupFailed, Some("notes".to_owned()))]
    );
}

#[tokio::test]
async fn a_retained_workspace_records_its_cleanup_as_the_cause() {
    let manager = Fixture::new().await;
    let root = &manager.root;
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
    (manager
        .spaces
        .announce_removal(&*manager, &workspace, Cleanup::Merged, &removal))
    .await
    .unwrap();

    assert_eq!(retentions(&manager).await, [Some(EventCause::Merged)]);
    assert_eq!(
        notices(&manager).await,
        [(AlertKind::CleanupFailed, Some("topic".to_owned()))]
    );
}

#[test]
fn every_removed_space_with_a_record_is_restorable() {
    let (id, name) = (SpaceId::from("id"), SpaceName::from("topic"));
    let space = |kind: SpaceKind| Announced {
        kind,
        id: &id,
        name: &name,
        worktree: kind.has_worktree(),
    };
    let restorable =
        |kind, cleanup| (space(kind).removal_notice(cleanup)).contains("shoal restore id");
    assert!(restorable(SpaceKind::Workspace, Cleanup::Idle));
    assert!(restorable(SpaceKind::Workspace, Cleanup::Parent("parent")));
    assert!(!restorable(SpaceKind::Workspace, Cleanup::DeletedWorktree));
    assert!(restorable(SpaceKind::Chat, Cleanup::Parent("parent")));
    assert!(restorable(SpaceKind::Chat, Cleanup::Idle));
}
