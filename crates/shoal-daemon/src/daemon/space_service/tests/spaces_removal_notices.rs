use crate::daemon::manager::Manager;
use crate::daemon::notifications::AlertKind;
use crate::daemon::spaces::removal_notices::*;
use crate::model::EventCause;
use crate::model::Space;
use anyhow::Result;

use crate::daemon::{
    events::{EventItem, EventKind},
    test_support::manager,
};

/// The causes of the retentions the journal records.
async fn retentions(manager: &Manager) -> Vec<Option<EventCause>> {
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

async fn notices(manager: &Manager) -> Vec<(AlertKind, Option<String>)> {
    let notifications = manager.notifications.list(false, 100).await.unwrap();
    notifications
        .into_iter()
        .map(|notification| (notification.kind, notification.space.map(String::from)))
        .collect()
}

#[tokio::test]
async fn a_retained_chat_records_its_cleanup_as_the_cause() {
    let (_root, manager) = manager().await;
    let chat = crate::daemon::test_support::chat(&manager, "notes", "chat", None, None).await;

    let removal: Result<bool> = Err(anyhow::anyhow!("agent running"));
    let space: Space = chat.into();
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
