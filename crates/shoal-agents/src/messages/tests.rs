use shoal_daemon_base::alerts::{self, AlertKind, MAX_UNREAD_BYTES, NewAlert};
use shoal_protocol::{model::Workspace, space_names::SpaceSelector};

use super::Delivery;
use crate::stand_in::Fixture;

/// Check that what the agent of `workspace` receives of its unread alerts
/// stays within their limit.
async fn assert_fits_one_delivery(fixture: &Fixture, workspace: &Workspace) {
    let unread = fixture
        .agents
        .unread_messages(&workspace.id)
        .await
        .unwrap()
        .alerts;
    let delivered = serde_json::to_vec(&unread).unwrap();
    assert!(
        delivered.len() <= MAX_UNREAD_BYTES,
        "{} alerts take {} bytes",
        unread.len(),
        delivered.len()
    );
}

#[tokio::test]
async fn a_message_for_the_next_turn_wakes_only_once_queued_to_wake() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("receiver").await;
    let queue = |delivery| {
        fixture.agents.queue_agent_message_delivered(
            &workspace.id,
            AlertKind::Message,
            "Memory is high".into(),
            delivery,
        )
    };
    let wakes = || async {
        fixture
            .agents
            .unread_messages(&workspace.id)
            .await
            .unwrap()
            .wakes
    };
    // Work running in the workspace does not make it wake.
    fixture.execution(&workspace, "command", None).await;
    queue(Delivery::NextTurn).await.unwrap();
    assert!(!wakes().await);
    let mut changed = fixture.agents.signals.watch_messages();
    changed.borrow_and_update();
    queue(Delivery::NextTurn).await.unwrap();
    assert!(!changed.has_changed().unwrap());
    // Queued to wake, it wakes and stays so.
    queue(Delivery::Wake).await.unwrap();
    assert!(changed.has_changed().unwrap());
    assert!(wakes().await);
    queue(Delivery::NextTurn).await.unwrap();
    assert!(wakes().await);
    let space = SpaceSelector::id(&workspace.id);
    let unread = fixture
        .agents
        .agent_messages(&*fixture.daemon, &space)
        .await;
    assert_eq!(unread.unwrap().len(), 1);
}

#[tokio::test]
async fn unread_alerts_that_json_escapes_still_fit_one_delivery() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace(&"🦀".repeat(40)).await;
    let mut queued = 0;
    loop {
        let text = format!("{queued}{}", "\t".repeat(500));
        let alert = NewAlert::shoal(AlertKind::Message, Default::default(), text);
        if fixture
            .agents
            .queue_alert(&workspace.id, alert, false)
            .await
            .is_err()
        {
            break;
        }
        queued += 1;
    }
    assert!(queued > 0);
    assert_fits_one_delivery(&fixture, &workspace).await;
}

#[tokio::test]
async fn a_backlog_beyond_the_limit_reaches_its_agent_over_several_deliveries() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace(&"🦀".repeat(40)).await;
    // What an earlier release queued, which the migration keeps whatever
    // it takes once each alert names its recipient.
    let id = workspace.id.clone();
    (fixture.agents.store)
        .write(move |tx| {
            for n in 0..50 {
                let text = format!("{n}{}", "\t".repeat(500));
                let alert = NewAlert::shoal(AlertKind::Message, Default::default(), text);
                alerts::insert(tx, Some(&id), &alert, true, false)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let to = SpaceSelector::id(&workspace.id);
    let mut delivered = Vec::new();
    for _ in 0..50 {
        let unread = fixture
            .agents
            .unread_messages(&workspace.id)
            .await
            .unwrap()
            .alerts;
        if unread.is_empty() {
            break;
        }
        let bytes = serde_json::to_vec(&unread).unwrap().len();
        assert!(bytes <= MAX_UNREAD_BYTES, "{bytes} bytes");
        let ids: Vec<i64> = unread.iter().map(|alert| alert.id).collect();
        (fixture
            .agents
            .mark_agent_messages_delivered(&*fixture.daemon, &to, ids.clone()))
        .await
        .unwrap();
        delivered.extend(ids);
    }
    assert_eq!(delivered.len(), 50);
}
