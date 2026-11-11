use super::*;

fn loss(item: &str, lines: &[&str]) -> (String, Vec<String>) {
    (
        item.into(),
        lines.iter().map(|line| (*line).into()).collect(),
    )
}

#[test]
fn losses_name_their_item_only_when_the_action_covers_several() {
    assert_eq!(
        losses_text("Remove workspace: a", &[loss("a", &["Deletes: 1 commit"])]),
        "Remove workspace: a\nDeletes: 1 commit"
    );
    assert_eq!(
        losses_text(
            "Remove 3 workspaces: a, b, c",
            &[
                loss("a", &["Holds:", "  review"]),
                loss("b", &[]),
                loss("c", &["Deletes: uncommitted changes"]),
            ]
        ),
        "Remove 3 workspaces: a, b, c\na:\n  Holds:\n    review\nc:\n  Deletes: uncommitted changes"
    );
}
