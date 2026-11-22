use serde_json::json;

use super::*;

fn told(files: &[&str]) -> Told {
    serde_json::from_value(json!({
        "above": "coordinator", "about": {"space": "child", "name": "child"},
        "for_user": true,
        "record": {"alert": {"kind": "workspace_done", "message": "child: done", "files": files}},
    }))
    .unwrap()
}

#[test]
fn the_outbox_sends_in_the_order_events_came() {
    let mut db = shoal_store::test_support::migrated().unwrap();
    let tx = db.transaction().unwrap();
    for message in ["first", "second"] {
        let mut event = told(&[]);
        event.record = ToldRecord::Alert {
            kind: AlertKind::SpaceDone,
            message: message.into(),
            files: Vec::new(),
        };
        push_outbox(&tx, &Call::Tell { told: event }, UnixTime::now()).unwrap();
    }
    let mut sent = Vec::new();
    while let Some((seq, call)) = first_outbox(&tx).unwrap() {
        let Call::Tell { told } = call else {
            panic!("the outbox held {call:?}");
        };
        sent.push(told.record.texts().1);
        remove_outbox(&tx, seq).unwrap();
    }
    assert_eq!(sent, ["first", "second"]);
}

#[tokio::test]
async fn a_message_no_coordinator_takes_reaches_the_user_from_its_space() {
    let root = tempfile::tempdir().unwrap();
    let hub = crate::stand_in::hub(&root, shoal_core::config::Config::default()).await;
    // A message of a child on another machine, with no agent above it.
    let mut event = told(&[]);
    event.record = ToldRecord::Alert {
        kind: AlertKind::Message,
        message: "from elsewhere".into(),
        files: Vec::new(),
    };
    (hub.deliver_told(&crate::stand_in::StandIn::default(), event))
        .await
        .unwrap();
    let alerts = hub.notifications.list(true, 10).await.unwrap();
    let [alert] = alerts.as_slice() else {
        panic!("{alerts:?}");
    };
    // The space that sent it, which may withdraw it.
    assert_eq!(
        (alert.sender, alert.space_id.as_ref().map(|id| id.as_str())),
        (Sender::Space, Some("child"))
    );
}
