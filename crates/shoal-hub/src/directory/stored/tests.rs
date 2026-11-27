use serde_json::json;

use super::*;

#[test]
fn a_summary_round_trips_with_its_id_and_number_out_of_its_json() {
    let summary = json!({
        "id": "work", "number": 7, "name": "work", "kind": "chat", "status": "working",
        "agent": true,
    });
    let space: SpaceSummary = serde_json::from_value(summary.clone()).unwrap();
    let stored = Stored::of(&space).unwrap();
    assert_eq!(stored.number, 7);
    let kept: Value = serde_json::from_str(&stored.summary).unwrap();
    assert_eq!((kept.get(ID), kept.get(NUMBER)), (None, None));
    let read = stored.summary(&space.id).unwrap();
    assert_eq!(serde_json::to_value(read).unwrap(), summary);
}
