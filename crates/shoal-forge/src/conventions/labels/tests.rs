use super::*;
use serde_json::json;

#[test]
fn labels_read_descriptions_and_exclusivity_per_forge() {
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    assert_eq!(
        forgejo.label(&json!({"name": "type/bug", "description": "Broken", "exclusive": true})),
        Some(Label {
            name: "type/bug".into(),
            description: "Broken".into(),
            exclusive: true,
        })
    );
    let github = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    let label = github
        .label(&json!({"name": "bug", "description": null}))
        .unwrap();
    assert_eq!((label.description.as_str(), label.exclusive), ("", false));
    let gitlab = ForgeRepo::parse("git@gitlab.com:group/repo.git").unwrap();
    assert!(
        gitlab
            .label(&json!({"name": "priority::high"}))
            .unwrap()
            .exclusive
    );
    assert!(!gitlab.label(&json!({"name": "bug"})).unwrap().exclusive);
    assert_eq!(forgejo.label(&json!({"id": 3})), None);
}
