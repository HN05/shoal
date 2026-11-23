use super::*;
use crate::alerts::RETAINED;

/// No space has a coordinator.
struct Nobody;

impl Coordinators for Nobody {
    fn tell<'a>(
        &'a self,
        _alert: NewAlert,
        _for_user: bool,
        _source: Source,
    ) -> BoxFuture<'a, Option<Sent>> {
        Box::pin(async { None })
    }
}

/// The notifications of a store in a temporary state directory.
async fn notifications() -> (tempfile::TempDir, Notifications) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = shoal_store::test_support::unsynced_store(path)
        .await
        .unwrap();
    (temp, Notifications::new(store))
}

#[tokio::test]
async fn pruning_keeps_unread_and_the_newest_read_notifications() {
    let (_temp, notifications) = notifications().await;
    let notify = |kind, message: String| {
        notifications.notify(&Nobody, NewAlert::shoal(kind, About::default(), message))
    };
    notify(AlertKind::SpaceRemoved, "keep unread".into()).await;
    for i in 0..RETAINED + 5 {
        notify(AlertKind::AgentExited, format!("exit {i}")).await;
    }
    notifications
        .store
        .run(|db| {
            db.execute("UPDATE alerts SET read=1 WHERE id>1", [])?;
            Ok(())
        })
        .await
        .unwrap();
    notify(AlertKind::AgentExited, "final".into()).await;
    // Read rows beyond the newest RETAINED go; the older unread one stays.
    let all = notifications.list(false, u32::MAX).await.unwrap();
    assert_eq!(all.len(), RETAINED + 1);
    assert_eq!(all[0].message, "keep unread");
    assert_eq!(all[1].message, "exit 6");
    assert_eq!(all.iter().filter(|n| !n.read).count(), 2);
}

/// An unread alert to the user that the machine `machine` keeps, recorded
/// at `created_at`.
fn kept_on(machine: &str, id: i64, created_at: i64) -> Alert {
    Alert {
        id,
        created_at: crate::time::UnixTime::from_seconds(created_at),
        kind: AlertKind::Message,
        sender: shoal_protocol::alerts::Sender::User,
        recipient: None,
        space: None,
        space_id: None,
        number: None,
        message: format!("from {machine}"),
        read: false,
        files: Vec::new(),
        machine: Some(shoal_protocol::hub::Machine {
            id: machine.to_owned().into(),
            name: machine.into(),
        }),
    }
}

#[tokio::test]
async fn other_machines_alerts_join_this_machine_s_in_the_order_they_were_recorded() {
    let (_temp, notifications) = notifications().await;
    let message = NewAlert::shoal(AlertKind::Message, About::default(), "here".into());
    notifications.notify(&Nobody, message).await;
    let whole = vec![kept_on("desk", 7, 0), kept_on("desk", 9, i64::MAX / 2)];
    notifications
        .take_copies(AlertCopies::Whole(whole))
        .await
        .unwrap();
    let listed = notifications.list(false, 10).await.unwrap();
    let order: Vec<(Option<String>, i64)> = (listed.iter())
        .map(|alert| (alert.machine.as_ref().map(|m| m.name.clone()), alert.id))
        .collect();
    assert_eq!(
        order,
        [
            (Some("desk".into()), 7),
            (None, 1),
            (Some("desk".into()), 9)
        ]
    );
    assert_eq!(notifications.unread().await.unwrap(), 3);
    // A follow past the desk's 7 and this machine's 1 has only the desk's
    // 9 left, whose number another machine's alerts do not order.
    let after: AlertCursor = "1,desk=7".parse().unwrap();
    let left = notifications.after(&after, 10).await.unwrap();
    assert_eq!(left.iter().map(|alert| alert.id).collect::<Vec<_>>(), [9]);
    let dropped = AlertCopies::Dropped {
        machine: "desk".to_owned().into(),
    };
    notifications.take_copies(dropped).await.unwrap();
    assert_eq!(notifications.unread().await.unwrap(), 1);
}

#[tokio::test]
async fn a_follow_cut_short_passes_none_of_a_machine_s_numbers_it_did_not_deliver() {
    let (_temp, notifications) = notifications().await;
    // The desk's 9, renumbered, kept the time it was recorded at, before
    // its 5 and 6.
    let whole = vec![
        kept_on("desk", 9, 0),
        kept_on("desk", 5, 1),
        kept_on("desk", 6, 2),
    ];
    let copies = AlertCopies::Whole(whole);
    notifications.take_copies(copies).await.unwrap();
    let mut cursor = AlertCursor::default();
    let mut delivered = Vec::new();
    loop {
        let batch = notifications.after(&cursor, 1).await.unwrap();
        let Some(alert) = batch.first() else { break };
        cursor.deliver(alert);
        delivered.push(alert.id);
    }
    assert_eq!(delivered, [5, 6, 9]);
}
