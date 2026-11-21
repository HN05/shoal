use super::*;

#[test]
fn review_ids_are_positive_integers() {
    assert_eq!("17".parse::<ReviewId>().unwrap(), ReviewId(17));
    assert_eq!(serde_json::to_string(&ReviewId(17)).unwrap(), "17");
    assert_eq!(
        serde_json::from_str::<ReviewId>("17").unwrap(),
        ReviewId(17)
    );
    for text in ["0", "-1", "a", "17.2"] {
        assert!(text.parse::<ReviewId>().is_err(), "{text}");
    }
}

/// A review thread `item` of space 2 whose finding is `severity`.
fn thread(item: u32, severity: &str) -> Thread {
    serde_json::from_value(serde_json::json!({
        "id": item, "space_number": 2, "item": item, "space_id": "w1", "space": "cache",
        "kind": "review", "finding": {"severity": severity, "review": 1},
        "name": "Unchecked index", "opener": "shoal",
        "holder": {"kind": "space", "id": "w1"}, "created_at": 0,
    }))
    .unwrap()
}

#[test]
fn status_counts_findings_by_severity() {
    let threads = [
        thread(1, "should-fix"),
        thread(2, "blocker"),
        thread(3, "should-fix"),
    ];
    assert_eq!(severity_counts(&threads), "1 blocker, 2 should-fix");
    assert_eq!(severity_counts(&[]), "");
}

#[test]
fn a_finished_review_counts_the_findings_it_reopened() {
    let mut review: Review = serde_json::from_value(serde_json::json!({
        "id": 4, "workspace_id": "w1", "workspace": "cache", "target": {"kind": "workspace"},
        "agent": "claude", "state": "finished", "created_at": 0,
    }))
    .unwrap();
    assert_eq!(review.status(), "finished: no findings");
    review.reopened.push(thread(1, "nit"));
    assert_eq!(review.status(), "finished: 1 reopened");
    review.threads.push(thread(2, "blocker"));
    assert_eq!(review.status(), "finished: 1 blocker, 1 reopened");
}
