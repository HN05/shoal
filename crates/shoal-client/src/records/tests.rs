use super::*;

#[test]
fn a_record_is_tagged_with_its_kind_and_keyed_by_its_id() {
    let record = Record::Question(Question {
        id: 4,
        workspace_id: "w".into(),
        workspace: "api".into(),
        question: "Which?".into(),
        recommendation: "A".into(),
        reason: None,
        choices: Vec::new(),
        created_at: 1,
    });
    let value = serde_json::to_value(&record).unwrap();
    assert_eq!(value["type"], "question");
    assert_eq!(serde_json::from_value::<Record>(value).unwrap(), record);
    assert_eq!(
        record.key(),
        Some(RecordKey {
            kind: RecordKind::Question,
            id: "4".into()
        })
    );
}

#[test]
fn a_record_with_its_own_kind_field_round_trips() {
    let record = Record::Notification(Notification {
        id: 2,
        kind: "question_asked".into(),
        workspace: None,
        message: "Which?".into(),
        read: false,
        created_at: 1,
    });
    let value = serde_json::to_value(&record).unwrap();
    assert_eq!(
        (&value["type"], &value["kind"]),
        (&"notification".into(), &"question_asked".into())
    );
    assert_eq!(serde_json::from_value::<Record>(value).unwrap(), record);
}
