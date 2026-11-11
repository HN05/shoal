use super::*;

#[test]
fn every_status_survives_its_columns() {
    for status in [
        QuestionStatus::Open,
        QuestionStatus::Answered {
            answer: "SQLite".into(),
            answered_at: UnixTime::from_seconds(10),
            message_id: Some(4),
        },
        QuestionStatus::Answered {
            answer: "SQLite".into(),
            answered_at: UnixTime::from_seconds(10),
            message_id: None,
        },
        QuestionStatus::Closed {
            closed_at: UnixTime::from_seconds(11),
        },
        QuestionStatus::Withdrawn {
            withdrawn_at: UnixTime::from_seconds(12),
        },
    ] {
        assert_eq!(StatusColumns::from(&status).status(), Some(status));
    }
}

#[test]
fn a_withdrawal_is_stored_as_a_close() {
    assert_eq!(
        StatusColumns::from(&QuestionStatus::Withdrawn {
            withdrawn_at: UnixTime::from_seconds(12)
        }),
        StatusColumns {
            answer: None,
            answered_at: None,
            message_id: None,
            closed_at: Some(UnixTime::from_seconds(12)),
            withdrawn: true,
        }
    );
}

#[test]
fn an_answer_without_its_time_has_no_status() {
    let columns = StatusColumns {
        answer: Some("SQLite".into()),
        answered_at: None,
        message_id: None,
        closed_at: None,
        withdrawn: false,
    };
    assert_eq!(columns.status(), None);
}
