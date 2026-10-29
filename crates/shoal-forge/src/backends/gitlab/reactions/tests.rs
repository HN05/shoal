use super::*;
use serde_json::json;

#[test]
fn award_emoji_take_github_names_where_github_has_the_reaction() {
    let listed = |name: &str| {
        listed(&json!({"id": 3, "name": name, "user": {"username": "ann"}}))
            .unwrap()
            .content
    };
    assert_eq!(listed("thumbsup"), "+1");
    assert_eq!(listed("tada"), "hooray");
    assert_eq!(listed("heart"), "heart");
    assert_eq!(listed("100"), "100");
    let award = super::listed(&json!({"id": 3, "name": "eyes", "user": {"username": "ann"}}));
    assert_eq!(award.unwrap().login, "ann");
    assert!(super::listed(&json!({"id": 3})).is_err());
}

#[test]
fn award_emoji_are_added_by_gitlab_name_and_removed_by_id() {
    let added = add("notes/9/award_emoji".into(), "+1");
    assert_eq!(added.body, Some(json!({"name": "thumbsup"})));
    assert_eq!(
        add("award_emoji".into(), "100").body,
        Some(json!({"name": "100"}))
    );
    let own = |id: Value| Listed {
        id,
        content: "+1".into(),
        login: "ann".into(),
    };
    let removed = remove("award_emoji".into(), &own(json!(12))).unwrap();
    assert_eq!(
        (removed.method, removed.endpoint.as_str()),
        (HttpMethod::Delete, "award_emoji/12")
    );
    assert!(remove("award_emoji".into(), &own(Value::Null)).is_err());
    assert_eq!(github_name("thumbsdown"), "-1");
}
