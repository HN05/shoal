use super::*;

#[test]
fn notifications_name_the_number_each_form_gave() {
    let number = |old| {
        (old == 5).then_some(ItemNumber {
            space: 3.into(),
            item: 2,
        })
    };
    assert_eq!(
        renumbered(
            "approval 5: open issue \"x\"; approve with shoal approvals --approve 5",
            APPROVAL_FORMS,
            number
        )
        .as_deref(),
        Some("approval 3.2: open issue \"x\"; approve with shoal approvals --approve 3.2")
    );
    assert_eq!(
        renumbered(
            "question 5 (answer with shoal questions 5): Which question 55?",
            QUESTION_FORMS,
            number
        )
        .as_deref(),
        Some("question 3.2 (answer with shoal questions 3.2): Which question 55?")
    );
    assert_eq!(
        renumbered(
            "withdrew question 5 (no answer needed): Which?",
            QUESTION_FORMS,
            number
        )
        .as_deref(),
        Some("withdrew question 3.2 (no answer needed): Which?")
    );
    // A replacement that ends in the old number is not rewritten again.
    assert_eq!(
        renumbered(
            "withdrew question 1 (no answer needed): Which?",
            QUESTION_FORMS,
            |old| Some(ItemNumber {
                space: 1.into(),
                item: u32::try_from(old).ok()?
            })
        )
        .as_deref(),
        Some("withdrew question 1.1 (no answer needed): Which?")
    );
    // A number nothing maps is left as it was told.
    assert_eq!(
        renumbered("approval 7: hold the workspace", APPROVAL_FORMS, number),
        None
    );
}
