use rusqlite::Connection;
use serde_json::json;

use super::*;

fn told(files: &[&str]) -> Told {
    serde_json::from_value(json!({
        "above": "coordinator", "space": "child", "name": "child",
        "message": "child: done", "files": files,
    }))
    .unwrap()
}

#[test]
fn an_event_names_the_files_that_stay_on_its_machine() {
    assert_eq!(with_files(&told(&[])), "child: done");
    let message = with_files(&told(&["report.md", "chart.png"]));
    assert!(message.starts_with("child: done\n"), "{message}");
    assert!(message.ends_with("report.md, chart.png"), "{message}");
}

#[test]
fn the_outbox_sends_in_the_order_events_came() {
    let mut db = Connection::open_in_memory().unwrap();
    for schema in [
        include_str!("../../store/hub.sql"),
        include_str!("../../store/hub_routing.sql"),
    ] {
        db.execute_batch(schema).unwrap();
    }
    let tx = db.transaction().unwrap();
    for message in ["first", "second"] {
        let mut event = told(&[]);
        event.message = message.into();
        push_outbox(&tx, &Call::Tell { told: event }, UnixTime::now()).unwrap();
    }
    let mut sent = Vec::new();
    while let Some((seq, call)) = first_outbox(&tx).unwrap() {
        let Call::Tell { told } = call else {
            panic!("the outbox held {call:?}");
        };
        sent.push(told.message);
        remove_outbox(&tx, seq).unwrap();
    }
    assert_eq!(sent, ["first", "second"]);
}
