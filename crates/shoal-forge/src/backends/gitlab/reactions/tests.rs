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
