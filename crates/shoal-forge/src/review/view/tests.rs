use super::*;

#[test]
fn failed_views_omit_details_and_round_trip() {
    let view = ItemView {
        url: "https://forge.example/o/r/issues/1".into(),
        kind: ItemKind::Issue,
        details: None,
        check_logs: Vec::new(),
        errors: vec!["lookup failed".into()],
    };
    let json = serde_json::to_value(&view).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 3);
    assert_eq!(serde_json::from_value::<ItemView>(json).unwrap(), view);
}
