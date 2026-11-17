use super::*;

#[test]
fn a_session_label_names_its_agent() {
    let place = Origin::created(HostName::Zmx, "work".to_owned());
    let session = Session {
        agent: Some("claude".into()),
        ..Session::open("work".into(), 1, &place)
    };
    assert_eq!(session.label(), "work  claude  attached");
    assert_eq!(Session::open("work".into(), 0, &place).label(), "work");
    assert_eq!(
        serde_json::to_value(&session).unwrap()["agent"],
        serde_json::json!("claude")
    );
}
