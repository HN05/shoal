use super::*;

#[test]
fn an_items_line_shows_its_labels_then_its_assignees() {
    let item = Item {
        kind: ItemKind::Issue,
        number: 7,
        url: "https://github.com/team/repo/issues/7".parse().unwrap(),
        title: "Bug".into(),
        state: ItemState::Open,
        labels: vec!["bug".into()],
        assignees: vec!["ann".into(), "bob".into()],
        milestone: None,
        pr: None,
    };
    assert_eq!(item_line(&item, 2), "#7   open    Bug  [bug]  @ann  @bob");
    let unassigned = Item {
        assignees: Vec::new(),
        ..item
    };
    assert_eq!(item_line(&unassigned, 1), "#7  open    Bug  [bug]");
}
