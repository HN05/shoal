use super::*;

#[test]
fn a_record_from_before_one_relation_per_item_records_each_item_once() {
    let item = |url: &str| LinkedItem {
        url: url.parse().unwrap(),
        title: None,
    };
    let (issue, run, other) = (
        "https://forge/o/r/issues/1",
        "https://forge/o/r/actions/runs/4",
        "https://forge/o/r/issues/5",
    );
    let links = SpaceLinks {
        issues: vec![item(issue)],
        prs: Vec::new(),
        opened: vec![item(issue), item(run)],
        related: vec![item(run), item(other)],
        assignees: Vec::new(),
    };
    assert_eq!(
        super::recorded(&links),
        [
            (Relation::Opened, item(run)),
            (Relation::Related, item(other))
        ]
    );
}
