use super::*;
use crate::questions::{Ask, QuestionStatus};

fn question(item: u32) -> Question {
    Question {
        number: ItemNumber {
            space: 7.into(),
            item,
        },
        space_id: "seven".into(),
        space: "seven".into(),
        created_at: crate::time::UnixTime::from_seconds(1),
        ask: Ask {
            question: "Which database?".into(),
            recommendation: "SQLite".into(),
            reason: None,
            choices: Vec::new(),
        },
        status: QuestionStatus::Open,
        in_chat: false,
        files: Vec::new(),
    }
}

fn answer(item: u32) -> Operation {
    Operation::Question(QuestionMethod::AnswerQuestion {
        number: ItemNumber {
            space: 7.into(),
            item,
        },
        answer: "SQLite".into(),
        shown: None,
    })
}

fn shown(operation: &Operation) -> Option<&Question> {
    match operation {
        Operation::Question(QuestionMethod::AnswerQuestion { shown, .. }) => shown.as_ref(),
        _ => None,
    }
}

#[test]
fn an_answer_by_number_names_the_question_its_space_lists() {
    let listed = [question(1), question(2)];
    let mut by_number = answer(2);
    assert!(by_number.names_unpinned_item());
    by_number.pin_item(&listed, &[]);
    assert_eq!(shown(&by_number), Some(&listed[1]));
    assert!(!by_number.names_unpinned_item());
    // One the space no longer lists stays by number, which its machine
    // refuses as unknown.
    let mut gone = answer(3);
    gone.pin_item(&listed, &[]);
    assert_eq!(shown(&gone), None);
    assert!(gone.names_unpinned_item());
}
