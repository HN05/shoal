use super::*;
use serde_json::json;

#[test]
fn allowed_reactions_read_a_list_of_names_or_none() {
    let only = |list| AllowedReactions::only(&list).unwrap();
    assert_eq!(
        only(json!(["+1", "heart"])),
        AllowedReactions {
            names: vec!["+1".into(), "heart".into()],
            accepts_other_names: false,
        }
    );
    assert!(only(json!(null)).names.is_empty());
    assert!(only(json!([])).names.is_empty());
    assert!(AllowedReactions::only(&json!([3])).is_err());
}

#[tokio::test]
async fn forges_without_reactions_refuse_to_list_them() {
    let bitbucket = ForgeRepo::parse("https://bitbucket.org/team/repo").unwrap();
    let refused = bitbucket
        .allowed_reactions(&Account::user(&Default::default()))
        .await
        .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "Shoal cannot react to issues, PRs and comments on Bitbucket Cloud; Bitbucket has no reactions"
    );
}
