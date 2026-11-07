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
    assert_eq!(json["status"], "answered");
    assert_eq!(json["answer"], "Postgres");
    assert_eq!(json["answered_at"], 5);
    assert_eq!(json["message_id"], 7);
    assert_eq!(serde_json::from_value::<Question>(json).unwrap(), answered);

    for (status, name, time) in [
        (QuestionStatus::Open, "open", None),
        (
            QuestionStatus::Closed {
                closed_at: crate::time::UnixTime::from_seconds(6),
            },
            "closed",
            Some("closed_at"),
        ),
        (
            QuestionStatus::Withdrawn {
                withdrawn_at: crate::time::UnixTime::from_seconds(6),
            },
            "withdrawn",
            Some("withdrawn_at"),
        ),
    ] {
        let question = question(status);
        let json = serde_json::to_value(&question).unwrap();
        assert_eq!(json["status"], name);
        assert_eq!(json.get("answer"), None);
        if let Some(time) = time {
            assert_eq!(json[time], 6);
        }
        assert_eq!(serde_json::from_value::<Question>(json).unwrap(), question);
    }
}
