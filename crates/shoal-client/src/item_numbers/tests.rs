use super::*;

#[test]
fn numbers_name_the_space_then_the_item() {
    let number: ItemNumber = "3.12".parse().unwrap();
    assert_eq!(
        number,
        ItemNumber {
            space: 3.into(),
            item: 12
        }
    );
    assert_eq!(number.to_string(), "3.12");
}

#[test]
fn a_bare_number_is_refused_with_the_full_form() {
    for invalid in ["5", "", "3.", ".2", "0.2", "3.0", "3.2.1", "a.b", "-3.2"] {
        assert!(invalid.parse::<ItemNumber>().is_err(), "{invalid}");
    }
}

#[test]
fn records_carry_both_numbers_and_their_text() {
    let number = ItemNumber {
        space: 3.into(),
        item: 2,
    };
    let json = serde_json::to_value(number).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"space_number": 3, "item": 2, "number": "3.2"})
    );
    assert_eq!(serde_json::from_value::<ItemNumber>(json).unwrap(), number);
}

#[test]
fn the_other_kind_names_its_command() {
    let number = ItemNumber {
        space: 3.into(),
        item: 2,
    };
    let approval = ItemKind::Approval.misdirected(number);
    assert!(approval.contains("shoal approvals 3.2"), "{approval}");
    let question = ItemKind::Question.misdirected(number);
    assert!(question.contains("shoal questions 3.2"), "{question}");
}
