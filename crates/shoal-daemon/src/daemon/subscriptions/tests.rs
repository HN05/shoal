use super::*;
use crate::{
    daemon::{notifications::AlertKind, test_daemon::TestDaemon},
    protocol::{self, Request, SubscriptionMethod},
};
use shoal_protocol::subscriptions::records::{Record, RecordKey, RecordKind, Topic};

fn repository(id: &str, name: &str) -> (RecordKey, (Record, serde_json::Value)) {
    let record = Record::Repository(crate::model::Repository {
        id: id.into(),
        path: "/repo".into(),
        source: "/repo".into(),
        last_used: 0,
        name: Some(name.into()),
        workspaces_dir: "/tmp/shoal/repo".into(),
        contribution: false,
    });
    let value = serde_json::to_value(&record).unwrap();
    (record.key(), (record, value))
}

#[test]
fn changes_name_each_new_changed_and_removed_record() {
    let before: Records = [
        repository("a", "a"),
        repository("b", "b"),
        repository("c", "c"),
    ]
    .into_iter()
    .collect();
    let after: Records = [
        repository("a", "a"),
        repository("b", "renamed"),
        repository("d", "d"),
    ]
    .into_iter()
    .collect();
    let named: Vec<_> = changes(&before, &after)
        .into_iter()
        .map(|body| match body {
            Body::RecordChanged(record) => format!("changed {}", record.key().id),
            Body::RecordRemoved(key) => {
                assert_eq!(key.kind, RecordKind::Repository);
                format!("removed {}", key.id)
            }
            body => panic!("unexpected {body:?}"),
        })
        .collect();
    assert_eq!(named, ["changed b", "changed d", "removed c"]);
}

#[tokio::test]
async fn a_large_snapshot_arrives_in_pages() {
    let daemon = TestDaemon::start().await;
    for n in 0..200 {
        let message = format!("{n}: {}", "x".repeat(1000));
        daemon
            .manager
            .notify(None, AlertKind::CleanupFailed, message)
            .await;
    }
    let stream = daemon.raw().await;
    let (reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    let subscribe = SubscriptionMethod::Subscribe {
        topics: vec![Topic::Alerts],
        log: None,
    };
    let request = Request {
        scope: None,
        ..Request::new(subscribe)
    };
    protocol::write(&mut writer, &request).await.unwrap();
    let mut frames = 0;
    let mut records = 0;
    loop {
        let response: protocol::Response = protocol::read_buffered(&mut reader).await.unwrap();
        let Body::Records(page) = response.body else {
            panic!("expected the snapshot: {response:?}");
        };
        frames += 1;
        records += page.len();
        if !response.more {
            break;
        }
    }
    assert!(frames > 1, "{frames} frames");
    assert_eq!(records, 200);
}
