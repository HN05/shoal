use super::*;

#[test]
fn a_selector_keeps_its_wire_forms_and_spells_an_id_apart() {
    let path = Selector::Path {
        value: "repo".into(),
        path: "/work/repo".into(),
    };
    let id = Selector::Id("repo-id".into());
    let wire = |selector: &Selector| serde_json::to_value(selector).unwrap();
    assert_eq!(
        wire(&Selector::Value("repo".into())),
        serde_json::json!("repo")
    );
    assert_eq!(
        wire(&path),
        serde_json::json!({"value": "repo", "path": "/work/repo"})
    );
    assert_eq!(wire(&id), serde_json::json!({"id": "repo-id"}));
    let read = |value: serde_json::Value| serde_json::from_value::<Selector>(value).unwrap();
    assert!(matches!(read(wire(&id)), Selector::Id(read) if read == "repo-id"));
    assert!(matches!(
        read(serde_json::json!("repo-id")),
        Selector::Value(_)
    ));
    assert!(matches!(read(wire(&path)), Selector::Path { .. }));
}
