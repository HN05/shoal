use super::*;

fn question(status: QuestionStatus) -> Question {
    Question {
        number: ItemNumber { space: 2, item: 3 },
        space_id: "w1".into(),
        space: "cache".into(),
        created_at: crate::time::UnixTime::from_seconds(1),
        ask: Ask {
            question: "Which database?".into(),
            recommendation: "SQLite".into(),
            reason: None,
            choices: Vec::new(),
        },
        status,
        in_chat: false,
        files: Vec::new(),
    }
}

#[test]
fn a_question_carries_its_status_beside_its_other_fields() {
    let answered = question(QuestionStatus::Answered {
        answer: "Postgres".into(),
        answered_at: crate::time::UnixTime::from_seconds(5),
        message_id: Some(7),
    });
    let json = serde_json::to_value(&answered).unwrap();
    assert_eq!(json["number"], "2.3");
    assert_eq!(json["status"], "answered");
    assert_eq!(json["answer"], "Postgres");
    assert_eq!(json["message_id"], 7);
    assert_eq!(serde_json::from_value::<Question>(json).unwrap(), answered);
}
