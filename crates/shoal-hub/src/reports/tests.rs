use serde_json::json;

use super::*;

fn summaries(spaces: &[(&str, &str)]) -> Sent {
    let summaries = (spaces.iter()).map(|(id, status)| {
        let summary: SpaceSummary = serde_json::from_value(json!({
            "id": id, "number": 1, "name": id, "kind": "chat", "status": status,
        }))
        .unwrap();
        (summary.id.clone(), summary)
    });
    comparable(summaries.collect()).unwrap()
}

#[test]
fn a_report_carries_only_what_changed() {
    let before = summaries(&[
        ("same", "working"),
        ("changed", "working"),
        ("gone", "waiting"),
    ]);
    let after = summaries(&[
        ("same", "working"),
        ("changed", "waiting"),
        ("new", "working"),
    ]);
    let Some(Report::Changes { changed, removed }) = changes(&before, &after) else {
        panic!("expected changes");
    };
    let mut changed: Vec<String> = changed.iter().map(|space| space.id.to_string()).collect();
    changed.sort();
    assert_eq!(changed, ["changed", "new"]);
    assert_eq!(removed, [SpaceId::from("gone".to_owned())]);
    assert!(changes(&after, &after).is_none());
}
